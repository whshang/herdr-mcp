import { test } from "node:test";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";

import { DeviceRegistryDO } from "../dist/device-registry-do.js";

const DEVICE_A = "dev_01J9Z6P8G2K4M6N8Q0RSTVWXYZ";
const DEVICE_B = "dev_01J9Z6P8G2K4M6N8Q0RSTVWXYY";
const PRINCIPAL_A = "oauth:controller-a";
const PRINCIPAL_B = "oauth:controller-b";

class FakeStorage {
  constructor(map = new Map()) { this.map = map; this.transactionSeq = 0; this.activeTransaction = null; this.writeLog = []; }
  async get(key) { return this.map.get(key); }
  async put(key, value) {
    this.map.set(key, structuredClone(value));
    if (this.activeTransaction !== null) this.writeLog.push({ key, transaction: this.activeTransaction });
  }
  async delete(key) { return this.map.delete(key); }
  async list({ prefix } = {}) { return new Map([...this.map].filter(([key]) => !prefix || key.startsWith(prefix))); }
  async transaction(fn) {
    const transaction = ++this.transactionSeq;
    this.activeTransaction = transaction;
    try { return await fn(this); }
    finally { this.activeTransaction = null; }
  }
}

function device(deviceId, authorization = "active") {
  return {
    device_id: deviceId,
    workstation_id: deviceId,
    name: deviceId,
    authorization,
    scheduling: "enabled",
    credential_id: null,
    enrolled_at_ms: 1,
    updated_at_ms: 1,
    revoked_at_ms: authorization === "revoked" ? 1 : null,
  };
}

function makeRegistry(storage = new FakeStorage()) {
  const state = { storage };
  const env = { LINK_SHARED_SECRET: "test-pepper-link-shared-secret-high-entropy-32b!!" };
  return { storage, registry: new DeviceRegistryDO(state, env) };
}

async function putDevice(storage, record) {
  await storage.put(`device:${record.device_id}`, record);
}

async function call(registry, method, params, principal = PRINCIPAL_A, nowMs = 1000, canForceTakeover = false) {
  const response = await registry.fetch(new Request("https://registry.internal/internal/devices/fleet-control", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ method, params, authority: { principal, can_force_takeover: canForceTakeover }, now_ms: nowMs }),
  }));
  return response.json();
}

async function createChain(registry, key = "chain-create", principal = PRINCIPAL_A) {
  return call(registry, "herdr_mcp.work_chain.create", { idempotency_key: key }, principal);
}

async function acquire(registry, chain, key, principal = PRINCIPAL_A, nowMs = 1000) {
  return call(registry, "herdr_mcp.planner_lease.acquire", {
    work_chain_id: chain.work_chain_id,
    expected_chain_revision: chain.revision,
    ttl_ms: 30000,
    idempotency_key: key,
  }, principal, nowMs);
}

test("work chain and planner lease enforce exclusive holder, renewal, release, privileged takeover and stale fencing", async () => {
  const { registry } = makeRegistry();
  const created = await createChain(registry);
  assert.equal(created.ok, true);
  assert.equal(created.chain.revision, 1);
  assert.equal(created.chain.creator_principal, PRINCIPAL_A);

  const first = await acquire(registry, created.chain, "lease-a");
  assert.equal(first.ok, true);
  assert.equal(first.planner_lease.generation, 1);

  const staleRevision = await call(registry, "herdr_mcp.planner_lease.renew", {
    work_chain_id: created.chain.work_chain_id,
    expected_chain_revision: created.chain.revision,
    expected_lease_generation: 1,
    ttl_ms: 30000,
    idempotency_key: "renew-stale-revision",
  }, PRINCIPAL_A, 2000);
  assert.equal(staleRevision.code, "chain_revision_conflict");

  const conflict = await call(registry, "herdr_mcp.planner_lease.acquire", {
    work_chain_id: created.chain.work_chain_id,
    expected_chain_revision: first.chain.revision,
    ttl_ms: 30000,
    idempotency_key: "lease-b-conflict",
  }, PRINCIPAL_B, 2000);
  assert.equal(conflict.code, "planner_lease_conflict");

  const wrongHolder = await call(registry, "herdr_mcp.planner_lease.renew", {
    work_chain_id: created.chain.work_chain_id,
    expected_chain_revision: first.chain.revision,
    expected_lease_generation: 1,
    ttl_ms: 30000,
    idempotency_key: "renew-wrong",
  }, PRINCIPAL_B, 2000);
  assert.equal(wrongHolder.code, "planner_lease_holder_mismatch");

  const wrongRelease = await call(registry, "herdr_mcp.planner_lease.release", {
    work_chain_id: created.chain.work_chain_id,
    expected_chain_revision: first.chain.revision,
    expected_lease_generation: 1,
    idempotency_key: "release-wrong",
  }, PRINCIPAL_B, 2000);
  assert.equal(wrongRelease.code, "planner_lease_holder_mismatch");

  const renewed = await call(registry, "herdr_mcp.planner_lease.renew", {
    work_chain_id: created.chain.work_chain_id,
    expected_chain_revision: first.chain.revision,
    expected_lease_generation: 1,
    ttl_ms: 30000,
    idempotency_key: "renew-a",
  }, PRINCIPAL_A, 2000);
  assert.equal(renewed.ok, true);
  assert.equal(renewed.planner_lease.generation, 1);

  const deniedTakeover = await call(registry, "herdr_mcp.planner_lease.takeover", {
    work_chain_id: created.chain.work_chain_id,
    expected_chain_revision: renewed.chain.revision,
    expected_lease_generation: 1,
    ttl_ms: 30000,
    idempotency_key: "takeover-b",
    reason: "controller handoff",
  }, PRINCIPAL_B, 3000);
  assert.equal(deniedTakeover.code, "planner_lease_takeover_forbidden");

  const takeoverParams = {
    work_chain_id: created.chain.work_chain_id,
    expected_chain_revision: renewed.chain.revision,
    expected_lease_generation: 1,
    ttl_ms: 30000,
    idempotency_key: "takeover-b",
    reason: "owner-authorized controller handoff",
  };
  const takeover = await call(registry, "herdr_mcp.planner_lease.takeover", takeoverParams, PRINCIPAL_B, 3000, true);
  assert.equal(takeover.ok, true);
  assert.equal(takeover.planner_lease.generation, 2);
  assert.deepEqual(takeover.takeover, {
    previous_holder_principal: PRINCIPAL_A,
    previous_generation: 1,
    new_holder_principal: PRINCIPAL_B,
    new_generation: 2,
    reason: "owner-authorized controller handoff",
    at_ms: 3000,
  });

  const takeoverReplay = await call(registry, "herdr_mcp.planner_lease.takeover", takeoverParams, PRINCIPAL_B, 3001, true);
  assert.equal(takeoverReplay.replayed, true);
  assert.equal(takeoverReplay.planner_lease.generation, 2);
  assert.equal(takeoverReplay.chain.revision, takeover.chain.revision);

  const stale = await call(registry, "herdr_mcp.planner_lease.release", {
    work_chain_id: created.chain.work_chain_id,
    expected_chain_revision: takeover.chain.revision,
    expected_lease_generation: 1,
    idempotency_key: "stale-release",
  }, PRINCIPAL_A, 4000);
  assert.equal(stale.code, "stale_lease_generation");

  const released = await call(registry, "herdr_mcp.planner_lease.release", {
    work_chain_id: created.chain.work_chain_id,
    expected_chain_revision: takeover.chain.revision,
    expected_lease_generation: 2,
    idempotency_key: "release-b",
  }, PRINCIPAL_B, 4000);
  assert.equal(released.ok, true);
  assert.equal(released.chain.planner_lease, null);
});

test("alpha.1 rejects caller-supplied evidence refs and successful chains remain reconstructable", async () => {
  const { storage, registry } = makeRegistry();

  for (const [suffix, ref] of [
    ["blank", "   "],
    ["absolute", "/Users/example/private"],
    ["url", "https://example.invalid/evidence"],
  ]) {
    const rejected = await call(registry, "herdr_mcp.work_chain.create", {
      idempotency_key: `chain-ref-${suffix}`,
      portable_evidence_refs: [ref],
    });
    assert.equal(rejected.code, "invalid_params");
    assert.equal(rejected.field, "portable_evidence_refs");
  }

  const created = await createChain(registry, "chain-readable");
  assert.equal(created.ok, true);
  assert.deepEqual(created.chain.portable_evidence_refs, []);

  const inspected = await call(registry, "herdr_mcp.work_chain.inspect", {
    work_chain_id: created.chain.work_chain_id,
  });
  assert.equal(inspected.ok, true);

  const acquired = await acquire(registry, created.chain, "chain-readable-acquire");
  assert.equal(acquired.ok, true);

  const legacyStoredChain = structuredClone(await storage.get(`fleet:chain:v1:${created.chain.work_chain_id}`));
  delete legacyStoredChain.portable_evidence_refs;
  await storage.put(`fleet:chain:v1:${created.chain.work_chain_id}`, legacyStoredChain);

  const reconstructed = makeRegistry(storage).registry;
  const afterReconstruction = await call(reconstructed, "herdr_mcp.work_chain.inspect", {
    work_chain_id: created.chain.work_chain_id,
  }, PRINCIPAL_B, 2000);
  assert.equal(afterReconstruction.ok, true);
  assert.deepEqual(afterReconstruction.chain.portable_evidence_refs, []);
});

test("alpha.2 compact checkpoint is planner-fenced, strictly portable, and reconstructable", async () => {
  const { storage, registry } = makeRegistry();
  const created = await createChain(registry, "checkpoint-chain");
  const lease = await acquire(registry, created.chain, "checkpoint-lease", PRINCIPAL_A, 1000);
  const checkpointJson = JSON.stringify({ goal: "alpha2", state: "ready" });
  const checkpointSha256 = createHash("sha256").update(checkpointJson).digest("hex");
  const validRef = {
    kind: "git_source",
    repo_id: "github.com/whshang/herdr-mcp",
    commit_sha: "0".repeat(40),
    repo_relative_path: "crates/herdr-mcp/src/state_store.rs",
    line_start: 1,
    line_end: 10,
    evidence_sha256: "a".repeat(64),
  };

  for (const [suffix, ref] of [
    ["absolute", { ...validRef, repo_relative_path: "/Users/example/private" }],
    ["url", { ...validRef, repo_relative_path: "https://example.invalid/evidence" }],
    ["short-sha", { ...validRef, commit_sha: "e9281b4" }],
    ["local-id", { evidence_id: "ev_0123456789abcdef0123456789abcdef", kind: "result", sha256: "a".repeat(64) }],
  ]) {
    const rejected = await call(registry, "herdr_mcp.work_chain.checkpoint.update", {
      work_chain_id: created.chain.work_chain_id,
      expected_chain_revision: lease.chain.revision,
      expected_lease_generation: lease.planner_lease.generation,
      expected_checkpoint_revision: 0,
      idempotency_key: `checkpoint-ref-${suffix}`,
      summary: "Alpha2 checkpoint",
      checkpoint_json: checkpointJson,
      checkpoint_sha256: checkpointSha256,
      portable_evidence_refs: [ref],
    }, PRINCIPAL_A, 1100);
    assert.equal(rejected.code, "invalid_portable_evidence_refs");
  }

  const badHash = await call(registry, "herdr_mcp.work_chain.checkpoint.update", {
    work_chain_id: created.chain.work_chain_id,
    expected_chain_revision: lease.chain.revision,
    expected_lease_generation: lease.planner_lease.generation,
    expected_checkpoint_revision: 0,
    idempotency_key: "checkpoint-bad-hash",
    summary: "Alpha2 checkpoint",
    checkpoint_json: checkpointJson,
    checkpoint_sha256: "f".repeat(64),
    portable_evidence_refs: [validRef],
  }, PRINCIPAL_A, 1150);
  assert.equal(badHash.code, "checkpoint_hash_mismatch");

  const updatedParams = {
    work_chain_id: created.chain.work_chain_id,
    expected_chain_revision: lease.chain.revision,
    expected_lease_generation: lease.planner_lease.generation,
    expected_checkpoint_revision: 0,
    idempotency_key: "checkpoint-valid",
    summary: "Alpha2 checkpoint",
    checkpoint_json: checkpointJson,
    checkpoint_sha256: checkpointSha256,
    portable_evidence_refs: [validRef],
  };
  const updated = await call(registry, "herdr_mcp.work_chain.checkpoint.update", updatedParams, PRINCIPAL_A, 1200);
  assert.equal(updated.ok, true);
  assert.equal(updated.chain.checkpoint_revision, 1);
  assert.equal(updated.chain.compact_checkpoint.revision, 1);
  assert.deepEqual(updated.chain.portable_evidence_refs, [validRef]);

  const replay = await call(registry, "herdr_mcp.work_chain.checkpoint.update", updatedParams, PRINCIPAL_A, 1201);
  assert.equal(replay.replayed, true);
  assert.equal(replay.chain.checkpoint_revision, 1);

  const stale = await call(registry, "herdr_mcp.work_chain.checkpoint.update", {
    ...updatedParams,
    expected_chain_revision: updated.chain.revision,
    idempotency_key: "checkpoint-stale",
  }, PRINCIPAL_A, 1300);
  assert.equal(stale.code, "checkpoint_revision_conflict");
  assert.equal(stale.actual, 1);

  const chainKey = `fleet:chain:v1:${created.chain.work_chain_id}`;
  const stored = await storage.get(chainKey);
  assert.equal(JSON.stringify(stored).includes("/Users/"), false);
  assert.equal(JSON.stringify(stored).includes("https://"), false);
  assert.equal(JSON.stringify(stored).includes("raw transcript"), false);

  const reconstructed = makeRegistry(storage).registry;
  const inspected = await call(reconstructed, "herdr_mcp.work_chain.inspect", {
    work_chain_id: created.chain.work_chain_id,
  }, PRINCIPAL_B, 1400);
  assert.equal(inspected.ok, true);
  assert.equal(inspected.chain.checkpoint_revision, 1);
  assert.equal(inspected.chain.compact_checkpoint.checkpoint_sha256, checkpointSha256);
  assert.deepEqual(inspected.chain.portable_evidence_refs, [validRef]);

  const chainWrite = [...storage.writeLog].reverse().find((entry) => entry.key === chainKey);
  assert.ok(chainWrite);
  assert.ok(storage.writeLog.some((entry) =>
    entry.transaction === chainWrite.transaction && entry.key.startsWith("fleet:idempotency:v1:")
  ));
});

test("alpha.2 compact checkpoint rejects updates after the planner lease expires", async () => {
  const { registry } = makeRegistry();
  const created = await createChain(registry, "checkpoint-expired-chain");
  const lease = await acquire(registry, created.chain, "checkpoint-expired-lease", PRINCIPAL_A, 1000);
  const checkpointJson = JSON.stringify({ goal: "alpha2", state: "expired" });
  const rejected = await call(registry, "herdr_mcp.work_chain.checkpoint.update", {
    work_chain_id: created.chain.work_chain_id,
    expected_chain_revision: lease.chain.revision,
    expected_lease_generation: lease.planner_lease.generation,
    expected_checkpoint_revision: 0,
    idempotency_key: "checkpoint-expired-update",
    summary: "Must not commit after lease expiry",
    checkpoint_json: checkpointJson,
    checkpoint_sha256: createHash("sha256").update(checkpointJson).digest("hex"),
    portable_evidence_refs: [],
  }, PRINCIPAL_A, 32001);
  assert.equal(rejected.code, "planner_lease_missing_or_expired");
});

test("expired lease is reclaimable with monotonically increasing generation", async () => {
  const { registry } = makeRegistry();
  const created = await createChain(registry, "expired-chain");
  const first = await acquire(registry, created.chain, "expired-acquire", PRINCIPAL_A, 1000);
  const reclaimed = await call(registry, "herdr_mcp.planner_lease.acquire", {
    work_chain_id: created.chain.work_chain_id,
    expected_chain_revision: first.chain.revision,
    ttl_ms: 30000,
    idempotency_key: "expired-reclaim",
  }, PRINCIPAL_B, 32000);
  assert.equal(reclaimed.ok, true);
  assert.equal(reclaimed.planner_lease.generation, 2);
});

test("idempotency replays identical mutation and rejects payload mismatch", async () => {
  const { registry } = makeRegistry();
  const first = await createChain(registry, "same-key");
  const replay = await createChain(registry, "same-key");
  assert.equal(replay.ok, true);
  assert.equal(replay.replayed, true);
  assert.equal(replay.chain.work_chain_id, first.chain.work_chain_id);

  const acquired = await acquire(registry, first.chain, "same-lease-key");
  assert.equal(acquired.ok, true);
  const acquiredReplay = await acquire(registry, first.chain, "same-lease-key");
  assert.equal(acquiredReplay.replayed, true);

  const mismatch = await call(registry, "herdr_mcp.planner_lease.acquire", {
    work_chain_id: first.chain.work_chain_id,
    expected_chain_revision: first.chain.revision,
    ttl_ms: 60000,
    idempotency_key: "same-lease-key",
  });
  assert.equal(mismatch.code, "idempotency_key_payload_mismatch");

  const otherPrincipal = await createChain(registry, "same-key", PRINCIPAL_B);
  assert.equal(otherPrincipal.ok, true);
  assert.notEqual(otherPrincipal.chain.work_chain_id, first.chain.work_chain_id);
});

test("idempotency admission quota stays bounded while existing control resources remain operable", async () => {
  const { registry } = makeRegistry();
  const first = await call(registry, "herdr_mcp.work_chain.create", { idempotency_key: "expiry-key" }, PRINCIPAL_A, 1000);
  const expired = await call(registry, "herdr_mcp.work_chain.create", { idempotency_key: "expiry-key" }, PRINCIPAL_A, 1000 + 24 * 60 * 60 * 1000 + 1);
  assert.equal(expired.ok, true);
  assert.equal(expired.replayed, undefined);
  assert.notEqual(expired.chain.work_chain_id, first.chain.work_chain_id);

  const bounded = makeRegistry();
  await putDevice(bounded.storage, device(DEVICE_A));
  await putDevice(bounded.storage, device(DEVICE_B));
  const chain = await call(bounded.registry, "herdr_mcp.work_chain.create", { idempotency_key: "bounded-control-chain" }, PRINCIPAL_A, 2000);
  const lease = await acquire(bounded.registry, chain.chain, "bounded-control-acquire", PRINCIPAL_A, 2001);
  const lane = await call(bounded.registry, "herdr_mcp.execution_lane.create", {
    work_chain_id: chain.chain.work_chain_id,
    expected_chain_revision: lease.chain.revision,
    expected_lease_generation: 1,
    idempotency_key: "bounded-control-lane",
    device_id: DEVICE_A,
    repo_id: "github.com/whshang/herdr-mcp",
    base_commit: "e9281b488e093f522020db2a2c6100d92b69499f",
    branch_ref: "feat/bounded-control",
    status: "active",
  }, PRINCIPAL_A, 2002);
  assert.equal(lane.ok, true);

  for (let index = 0; index < 254; index += 1) {
    const created = await call(bounded.registry, "herdr_mcp.work_chain.create", { idempotency_key: `bounded-admission-${index}` }, PRINCIPAL_A, 2100 + index);
    assert.equal(created.ok, true);
  }
  const saturated = await call(bounded.registry, "herdr_mcp.work_chain.create", { idempotency_key: "bounded-admission-overflow" }, PRINCIPAL_A, 3000);
  assert.equal(saturated.code, "idempotency_capacity_exceeded");
  assert.equal(saturated.quota_scope, "admission");
  assert.equal(saturated.live_records, 256);
  assert.equal(saturated.limit, 256);
  assert.ok(saturated.recover_after_ms > 0);

  const renewedParams = {
    work_chain_id: chain.chain.work_chain_id,
    expected_chain_revision: lane.chain.revision,
    expected_lease_generation: 1,
    ttl_ms: 30000,
    idempotency_key: "bounded-control-renew",
  };
  const renewed = await call(bounded.registry, "herdr_mcp.planner_lease.renew", renewedParams, PRINCIPAL_A, 3100);
  assert.equal(renewed.ok, true);
  const renewReplay = await call(bounded.registry, "herdr_mcp.planner_lease.renew", renewedParams, PRINCIPAL_A, 3101);
  assert.equal(renewReplay.replayed, true);

  const takeoverParams = {
    work_chain_id: chain.chain.work_chain_id,
    expected_chain_revision: renewed.chain.revision,
    expected_lease_generation: 1,
    ttl_ms: 30000,
    reason: "operator recovers a saturated admission plane",
    idempotency_key: "bounded-control-takeover",
  };
  const takeover = await call(bounded.registry, "herdr_mcp.planner_lease.takeover", takeoverParams, PRINCIPAL_B, 3200, true);
  assert.equal(takeover.ok, true);
  const takeoverReplay = await call(bounded.registry, "herdr_mcp.planner_lease.takeover", takeoverParams, PRINCIPAL_B, 3201, true);
  assert.equal(takeoverReplay.replayed, true);
  assert.equal(takeoverReplay.planner_lease.generation, takeover.planner_lease.generation);

  const reassigned = await call(bounded.registry, "herdr_mcp.execution_lane.update", {
    work_chain_id: chain.chain.work_chain_id,
    expected_chain_revision: takeover.chain.revision,
    expected_lease_generation: 2,
    lane_id: lane.lane.lane_id,
    expected_lane_generation: 1,
    reassign: true,
    device_id: DEVICE_B,
    status: "active",
    idempotency_key: "bounded-control-reassign",
  }, PRINCIPAL_B, 3300);
  assert.equal(reassigned.ok, true);

  const completed = await call(bounded.registry, "herdr_mcp.execution_lane.update", {
    work_chain_id: chain.chain.work_chain_id,
    expected_chain_revision: reassigned.chain.revision,
    expected_lease_generation: 2,
    lane_id: reassigned.lane.lane_id,
    expected_lane_generation: reassigned.lane.lane_generation,
    status: "completed",
    idempotency_key: "bounded-control-complete",
  }, PRINCIPAL_B, 3400);
  assert.equal(completed.ok, true);

  const released = await call(bounded.registry, "herdr_mcp.planner_lease.release", {
    work_chain_id: chain.chain.work_chain_id,
    expected_chain_revision: completed.chain.revision,
    expected_lease_generation: 2,
    idempotency_key: "bounded-control-release",
  }, PRINCIPAL_B, 3500);
  assert.equal(released.ok, true);

  const releaseChainWrite = [...bounded.storage.writeLog].reverse().find((entry) => entry.key === `fleet:chain:v1:${chain.chain.work_chain_id}`);
  assert.ok(releaseChainWrite);
  assert.ok(bounded.storage.writeLog.some((entry) => entry.transaction === releaseChainWrite.transaction && entry.key.startsWith("fleet:idempotency:v1:")));

  const records = await bounded.storage.list({ prefix: "fleet:idempotency:v1:" });
  assert.ok(records.size <= 512);

  const recoveredAdmission = await call(bounded.registry, "herdr_mcp.work_chain.create", {
    idempotency_key: "bounded-admission-after-expiry",
  }, PRINCIPAL_A, 2000 + 24 * 60 * 60 * 1000 + 1);
  assert.equal(recoveredAdmission.ok, true);
});

test("routine control saturation preserves critical recovery capacity for both Alpha.1 principals", async () => {
  const { storage, registry } = makeRegistry();
  await putDevice(storage, device(DEVICE_A));
  await putDevice(storage, device(DEVICE_B));

  const releaseChainCreated = await createChain(registry, "routine-release-chain", PRINCIPAL_A, 4000);
  const releaseLease = await acquire(registry, releaseChainCreated.chain, "routine-release-acquire", PRINCIPAL_A, 4001);

  const controlChainCreated = await createChain(registry, "routine-control-chain", PRINCIPAL_A, 4002);
  const controlLease = await acquire(registry, controlChainCreated.chain, "routine-control-acquire", PRINCIPAL_A, 4003);
  let laneState = await call(registry, "herdr_mcp.execution_lane.create", {
    work_chain_id: controlChainCreated.chain.work_chain_id,
    expected_chain_revision: controlLease.chain.revision,
    expected_lease_generation: 1,
    idempotency_key: "routine-control-lane",
    device_id: DEVICE_A,
    repo_id: "github.com/whshang/herdr-mcp",
    base_commit: "e9281b488e093f522020db2a2c6100d92b69499f",
    branch_ref: "feat/routine-control",
    status: "active",
  }, PRINCIPAL_A, 4004);
  assert.equal(laneState.ok, true);

  // Two acquires already occupy routine-control slots. Fill the remaining 94.
  for (let index = 0; index < 94; index += 1) {
    laneState = await call(registry, "herdr_mcp.execution_lane.update", {
      work_chain_id: controlChainCreated.chain.work_chain_id,
      expected_chain_revision: laneState.chain.revision,
      expected_lease_generation: 1,
      lane_id: laneState.lane.lane_id,
      expected_lane_generation: laneState.lane.lane_generation,
      status: "active",
      validation_summary: `routine-${index}`,
      idempotency_key: `routine-control-update-${index}`,
    }, PRINCIPAL_A, 4100 + index);
    assert.equal(laneState.ok, true);
  }

  const routineSaturated = await call(registry, "herdr_mcp.execution_lane.update", {
    work_chain_id: controlChainCreated.chain.work_chain_id,
    expected_chain_revision: laneState.chain.revision,
    expected_lease_generation: 1,
    lane_id: laneState.lane.lane_id,
    expected_lane_generation: laneState.lane.lane_generation,
    status: "active",
    validation_summary: "routine-overflow",
    idempotency_key: "routine-control-overflow",
  }, PRINCIPAL_A, 4300);
  assert.equal(routineSaturated.code, "idempotency_capacity_exceeded");
  assert.equal(routineSaturated.quota_scope, "control_routine_principal");
  assert.equal(routineSaturated.live_records, 96);
  assert.equal(routineSaturated.limit, 96);

  const renewed = await call(registry, "herdr_mcp.planner_lease.renew", {
    work_chain_id: controlChainCreated.chain.work_chain_id,
    expected_chain_revision: laneState.chain.revision,
    expected_lease_generation: 1,
    ttl_ms: 30000,
    idempotency_key: "routine-critical-renew",
  }, PRINCIPAL_A, 4301);
  assert.equal(renewed.ok, true);

  const releasedA = await call(registry, "herdr_mcp.planner_lease.release", {
    work_chain_id: releaseChainCreated.chain.work_chain_id,
    expected_chain_revision: releaseLease.chain.revision,
    expected_lease_generation: 1,
    idempotency_key: "routine-critical-release-a",
  }, PRINCIPAL_A, 4302);
  assert.equal(releasedA.ok, true);

  const takeover = await call(registry, "herdr_mcp.planner_lease.takeover", {
    work_chain_id: controlChainCreated.chain.work_chain_id,
    expected_chain_revision: renewed.chain.revision,
    expected_lease_generation: 1,
    ttl_ms: 30000,
    reason: "recover while the other principal routine quota is saturated",
    idempotency_key: "routine-critical-takeover-b",
  }, PRINCIPAL_B, 4303, true);
  assert.equal(takeover.ok, true);

  const reassigned = await call(registry, "herdr_mcp.execution_lane.update", {
    work_chain_id: controlChainCreated.chain.work_chain_id,
    expected_chain_revision: takeover.chain.revision,
    expected_lease_generation: 2,
    lane_id: laneState.lane.lane_id,
    expected_lane_generation: laneState.lane.lane_generation,
    reassign: true,
    device_id: DEVICE_B,
    status: "active",
    idempotency_key: "routine-critical-reassign-b",
  }, PRINCIPAL_B, 4304);
  assert.equal(reassigned.ok, true);

  const completed = await call(registry, "herdr_mcp.execution_lane.update", {
    work_chain_id: controlChainCreated.chain.work_chain_id,
    expected_chain_revision: reassigned.chain.revision,
    expected_lease_generation: 2,
    lane_id: reassigned.lane.lane_id,
    expected_lane_generation: reassigned.lane.lane_generation,
    status: "completed",
    idempotency_key: "routine-critical-complete-b",
  }, PRINCIPAL_B, 4305);
  assert.equal(completed.ok, true);

  const releasedB = await call(registry, "herdr_mcp.planner_lease.release", {
    work_chain_id: controlChainCreated.chain.work_chain_id,
    expected_chain_revision: completed.chain.revision,
    expected_lease_generation: 2,
    idempotency_key: "routine-critical-release-b",
  }, PRINCIPAL_B, 4306);
  assert.equal(releasedB.ok, true);
});

test("execution lanes require explicit active device and portable repo/branch identity", async () => {
  const { storage, registry } = makeRegistry();
  await putDevice(storage, device(DEVICE_A));
  await putDevice(storage, device(DEVICE_B));
  const created = await createChain(registry, "lane-chain");
  const lease = await acquire(registry, created.chain, "lane-lease");

  const laneA = await call(registry, "herdr_mcp.execution_lane.create", {
    work_chain_id: created.chain.work_chain_id,
    expected_chain_revision: lease.chain.revision,
    expected_lease_generation: 1,
    idempotency_key: "lane-a",
    device_id: DEVICE_A,
    repo_id: "https://github.com/WhShang/Herdr-MCP.git",
    base_commit: "e9281b488e093f522020db2a2c6100d92b69499f",
    branch_ref: "refs/heads/feat/lane-a",
    file_scope: ["edge/cloudflare/src"],
    runtime_scope: ["runtime:edge"],
  });
  assert.equal(laneA.ok, true);
  assert.equal(laneA.lane.repo_id, "github.com/whshang/herdr-mcp");
  assert.equal(laneA.lane.branch_ref, "feat/lane-a");
  assert.equal("path" in laneA.lane, false);

  const laneB = await call(registry, "herdr_mcp.execution_lane.create", {
    work_chain_id: created.chain.work_chain_id,
    expected_chain_revision: laneA.chain.revision,
    expected_lease_generation: 1,
    idempotency_key: "lane-b",
    device_id: DEVICE_B,
    repo_id: "github.com/whshang/herdr-mcp",
    base_commit: "e9281b488e093f522020db2a2c6100d92b69499f",
    branch_ref: "feat/lane-b",
  });
  assert.equal(laneB.ok, true);
  assert.notEqual(laneB.lane.device_id, laneA.lane.device_id);

  const conflict = await call(registry, "herdr_mcp.execution_lane.create", {
    work_chain_id: created.chain.work_chain_id,
    expected_chain_revision: laneB.chain.revision,
    expected_lease_generation: 1,
    idempotency_key: "lane-conflict",
    device_id: DEVICE_B,
    repo_id: "github.com/whshang/herdr-mcp",
    base_commit: "e9281b488e093f522020db2a2c6100d92b69499f",
    branch_ref: "feat/lane-a",
  });
  assert.equal(conflict.code, "branch_lane_conflict");

  const absolute = await call(registry, "herdr_mcp.execution_lane.create", {
    work_chain_id: created.chain.work_chain_id,
    expected_chain_revision: laneB.chain.revision,
    expected_lease_generation: 1,
    idempotency_key: "lane-absolute",
    device_id: DEVICE_B,
    repo_id: "/Users/whshang/Documents/herdr-mcp",
    base_commit: "e9281b488e093f522020db2a2c6100d92b69499f",
    branch_ref: "feat/path",
  });
  assert.equal(absolute.code, "invalid_lane_identity");

  const traversalScope = await call(registry, "herdr_mcp.execution_lane.create", {
    work_chain_id: created.chain.work_chain_id,
    expected_chain_revision: laneB.chain.revision,
    expected_lease_generation: 1,
    idempotency_key: "lane-traversal-scope",
    device_id: DEVICE_B,
    repo_id: "github.com/whshang/herdr-mcp",
    base_commit: "e9281b488e093f522020db2a2c6100d92b69499f",
    branch_ref: "feat/scope",
    file_scope: ["src/../outside"],
  });
  assert.equal(traversalScope.code, "invalid_params");
});

test("execution lane identity and scopes use portable canonical syntax", async () => {
  const { storage, registry } = makeRegistry();
  await putDevice(storage, device(DEVICE_A));
  const created = await createChain(registry, "portable-lane-chain");
  const lease = await acquire(registry, created.chain, "portable-lane-lease");
  const base = {
    work_chain_id: created.chain.work_chain_id,
    expected_chain_revision: lease.chain.revision,
    expected_lease_generation: 1,
    device_id: DEVICE_A,
    repo_id: "github.com/whshang/herdr-mcp",
    base_commit: "e9281b488e093f522020db2a2c6100d92b69499f",
    branch_ref: "feat/portable-lane",
  };

  const abbreviated = await call(registry, "herdr_mcp.execution_lane.create", {
    ...base,
    idempotency_key: "portable-abbreviated-sha",
    base_commit: "e9281b4",
  });
  assert.equal(abbreviated.code, "invalid_lane_identity");

  for (const [suffix, branchRef] of [
    ["tilde", "bad~branch"],
    ["caret", "bad^branch"],
    ["question", "bad?branch"],
    ["star", "bad*branch"],
    ["bracket", "bad[branch"],
    ["double-slash", "bad//branch"],
    ["leading-dot", ".bad"],
    ["lock", "bad.lock"],
  ]) {
    const rejected = await call(registry, "herdr_mcp.execution_lane.create", {
      ...base,
      idempotency_key: `portable-branch-${suffix}`,
      branch_ref: branchRef,
    });
    assert.equal(rejected.code, "invalid_lane_identity", branchRef);
  }

  const badRepo = await call(registry, "herdr_mcp.execution_lane.create", {
    ...base,
    idempotency_key: "portable-repo-dot-segment",
    repo_id: "github.com/whshang/./herdr-mcp",
  });
  assert.equal(badRepo.code, "invalid_lane_identity");

  for (const [suffix, fileScope] of [
    ["url", ["https://evil.example/task"]],
    ["shell", ["$(shell)"]],
    ["absolute", ["/tmp/task"]],
    ["empty-segment", ["src//task"]],
    ["traversal", ["src/../task"]],
  ]) {
    const rejected = await call(registry, "herdr_mcp.execution_lane.create", {
      ...base,
      idempotency_key: `portable-file-scope-${suffix}`,
      branch_ref: `feat/file-scope-${suffix}`,
      file_scope: fileScope,
    });
    assert.equal(rejected.code, "invalid_params", suffix);
  }

  for (const [suffix, runtimeScope] of [
    ["url", ["https://runtime.example"]],
    ["shell", ["$(runtime)"]],
  ]) {
    const rejected = await call(registry, "herdr_mcp.execution_lane.create", {
      ...base,
      idempotency_key: `portable-runtime-scope-${suffix}`,
      branch_ref: `feat/runtime-scope-${suffix}`,
      runtime_scope: runtimeScope,
    });
    assert.equal(rejected.code, "invalid_params", suffix);
  }

  const sha1Lane = await call(registry, "herdr_mcp.execution_lane.create", {
    ...base,
    idempotency_key: "portable-sha1",
    branch_ref: "refs/heads/feat/portable-sha1",
    file_scope: ["edge/cloudflare/src"],
    runtime_scope: ["runtime:edge"],
  });
  assert.equal(sha1Lane.ok, true);
  assert.equal(sha1Lane.lane.branch_ref, "feat/portable-sha1");

  const sha256Lane = await call(registry, "herdr_mcp.execution_lane.create", {
    ...base,
    expected_chain_revision: sha1Lane.chain.revision,
    idempotency_key: "portable-sha256",
    base_commit: "a".repeat(64),
    branch_ref: "feat/portable-sha256",
    file_scope: ["crates/herdr-mcp/src"],
    runtime_scope: ["service:herdr-mcp"],
  });
  assert.equal(sha256Lane.ok, true);

  const validationRef = await call(registry, "herdr_mcp.execution_lane.create", {
    ...base,
    expected_chain_revision: sha256Lane.chain.revision,
    idempotency_key: "portable-validation-ref",
    branch_ref: "feat/portable-validation-ref",
    validation_refs: ["https://example.invalid/validation"],
  });
  assert.equal(validationRef.code, "invalid_params");
  assert.equal(validationRef.field, "validation_refs");

  const legacyStoredLane = structuredClone(await storage.get(`fleet:lane:v1:${sha256Lane.lane.lane_id}`));
  delete legacyStoredLane.validation_refs;
  await storage.put(`fleet:lane:v1:${sha256Lane.lane.lane_id}`, legacyStoredLane);

  const reconstructed = makeRegistry(storage).registry;
  const inspected = await call(reconstructed, "herdr_mcp.execution_lane.inspect", {
    lane_id: sha256Lane.lane.lane_id,
  }, PRINCIPAL_B, 5000);
  assert.equal(inspected.ok, true);
  assert.deepEqual(inspected.lane.validation_refs, []);
});

test("unknown, revoked, and missing devices fail closed without auto-routing", async () => {
  const { storage, registry } = makeRegistry();
  await putDevice(storage, device(DEVICE_A, "revoked"));
  const created = await createChain(registry, "device-chain");
  const lease = await acquire(registry, created.chain, "device-lease");
  const base = {
    work_chain_id: created.chain.work_chain_id,
    expected_chain_revision: lease.chain.revision,
    expected_lease_generation: 1,
    repo_id: "github.com/whshang/herdr-mcp",
    base_commit: "e9281b488e093f522020db2a2c6100d92b69499f",
    branch_ref: "feat/device-gate",
  };
  const missing = await call(registry, "herdr_mcp.execution_lane.create", { ...base, idempotency_key: "missing" });
  assert.equal(missing.code, "device_id_required");
  const unknown = await call(registry, "herdr_mcp.execution_lane.create", { ...base, idempotency_key: "unknown", device_id: DEVICE_B });
  assert.equal(unknown.code, "device_not_found");
  const revoked = await call(registry, "herdr_mcp.execution_lane.create", { ...base, idempotency_key: "revoked", device_id: DEVICE_A });
  assert.equal(revoked.code, "device_not_authorized");
});

test("lane lifecycle is fenced, reassign is explicit, and terminal lanes cannot reactivate", async () => {
  const { storage, registry } = makeRegistry();
  await putDevice(storage, device(DEVICE_A));
  await putDevice(storage, device(DEVICE_B));
  const created = await createChain(registry, "lane-state-chain");
  const lease = await acquire(registry, created.chain, "lane-state-lease");

  const planned = await call(registry, "herdr_mcp.execution_lane.create", {
    work_chain_id: created.chain.work_chain_id,
    expected_chain_revision: lease.chain.revision,
    expected_lease_generation: 1,
    idempotency_key: "lane-state-create",
    device_id: DEVICE_A,
    repo_id: "github.com/whshang/herdr-mcp",
    base_commit: "e9281b488e093f522020db2a2c6100d92b69499f",
    branch_ref: "feat/lane-state",
  });
  assert.equal(planned.ok, true);

  const invalid = await call(registry, "herdr_mcp.execution_lane.update", {
    work_chain_id: created.chain.work_chain_id,
    expected_chain_revision: planned.chain.revision,
    expected_lease_generation: 1,
    lane_id: planned.lane.lane_id,
    expected_lane_generation: 1,
    status: "completed",
    idempotency_key: "lane-state-invalid",
  });
  assert.equal(invalid.code, "lane_transition_invalid");

  const active = await call(registry, "herdr_mcp.execution_lane.update", {
    work_chain_id: created.chain.work_chain_id,
    expected_chain_revision: planned.chain.revision,
    expected_lease_generation: 1,
    lane_id: planned.lane.lane_id,
    expected_lane_generation: 1,
    status: "active",
    idempotency_key: "lane-state-active",
  });
  assert.equal(active.ok, true);
  assert.equal(active.lane.lane_generation, 2);

  const takeover = await call(registry, "herdr_mcp.planner_lease.takeover", {
    work_chain_id: created.chain.work_chain_id,
    expected_chain_revision: active.chain.revision,
    expected_lease_generation: 1,
    reason: "owner reassigns the lane",
    idempotency_key: "lane-state-takeover",
  }, PRINCIPAL_B, 4000, true);
  assert.equal(takeover.ok, true);

  const missingReassign = await call(registry, "herdr_mcp.execution_lane.update", {
    work_chain_id: created.chain.work_chain_id,
    expected_chain_revision: takeover.chain.revision,
    expected_lease_generation: 2,
    lane_id: active.lane.lane_id,
    expected_lane_generation: 2,
    status: "active",
    idempotency_key: "lane-state-owner-mismatch",
  }, PRINCIPAL_B, 5000);
  assert.equal(missingReassign.code, "execution_lane_owner_mismatch");

  const reassigned = await call(registry, "herdr_mcp.execution_lane.update", {
    work_chain_id: created.chain.work_chain_id,
    expected_chain_revision: takeover.chain.revision,
    expected_lease_generation: 2,
    lane_id: active.lane.lane_id,
    expected_lane_generation: 2,
    status: "active",
    reassign: true,
    device_id: DEVICE_B,
    idempotency_key: "lane-state-reassign",
  }, PRINCIPAL_B, 5000);
  assert.equal(reassigned.ok, true);
  assert.equal(reassigned.lane.owner_principal, PRINCIPAL_B);
  assert.equal(reassigned.lane.device_id, DEVICE_B);
  assert.equal(reassigned.lane.lane_generation, 3);

  const staleOldPlanner = await call(registry, "herdr_mcp.execution_lane.update", {
    work_chain_id: created.chain.work_chain_id,
    expected_chain_revision: reassigned.chain.revision,
    expected_lease_generation: 1,
    lane_id: reassigned.lane.lane_id,
    expected_lane_generation: 3,
    status: "blocked",
    idempotency_key: "lane-state-stale-planner",
  }, PRINCIPAL_A, 6000);
  assert.equal(staleOldPlanner.code, "stale_lease_generation");

  const completed = await call(registry, "herdr_mcp.execution_lane.update", {
    work_chain_id: created.chain.work_chain_id,
    expected_chain_revision: reassigned.chain.revision,
    expected_lease_generation: 2,
    lane_id: reassigned.lane.lane_id,
    expected_lane_generation: 3,
    status: "completed",
    idempotency_key: "lane-state-complete",
  }, PRINCIPAL_B, 6000);
  assert.equal(completed.ok, true);

  const reactivate = await call(registry, "herdr_mcp.execution_lane.update", {
    work_chain_id: created.chain.work_chain_id,
    expected_chain_revision: completed.chain.revision,
    expected_lease_generation: 2,
    lane_id: completed.lane.lane_id,
    expected_lane_generation: 4,
    status: "active",
    idempotency_key: "lane-state-reactivate",
  }, PRINCIPAL_B, 7000);
  assert.equal(reactivate.code, "lane_transition_invalid");

  const replacement = await call(registry, "herdr_mcp.execution_lane.create", {
    work_chain_id: created.chain.work_chain_id,
    expected_chain_revision: completed.chain.revision,
    expected_lease_generation: 2,
    idempotency_key: "lane-state-replacement",
    device_id: DEVICE_B,
    repo_id: "github.com/whshang/herdr-mcp",
    base_commit: "e9281b488e093f522020db2a2c6100d92b69499f",
    branch_ref: "feat/lane-state",
  }, PRINCIPAL_B, 7000);
  assert.equal(replacement.ok, true);
});

test("fleet authority survives DO instance reconstruction and leaves legacy device state readable", async () => {
  const storage = new FakeStorage();
  await putDevice(storage, device(DEVICE_A));
  const first = makeRegistry(storage).registry;
  const created = await createChain(first, "persist-chain");
  const acquired = await acquire(first, created.chain, "persist-lease");

  const second = makeRegistry(storage).registry;
  const inspected = await call(second, "herdr_mcp.work_chain.inspect", { work_chain_id: created.chain.work_chain_id }, PRINCIPAL_B, 2000);
  assert.equal(inspected.ok, true);
  assert.equal(inspected.chain.revision, acquired.chain.revision);
  assert.equal(inspected.chain.planner_lease.generation, 1);

  const deviceResponse = await second.fetch(new Request(`https://registry.internal/internal/devices/${DEVICE_A}`));
  assert.equal(deviceResponse.status, 200);
  assert.equal((await deviceResponse.json()).device.device_id, DEVICE_A);
});
