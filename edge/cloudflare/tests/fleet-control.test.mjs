import { test } from "node:test";
import assert from "node:assert/strict";

import { DeviceRegistryDO } from "../dist/device-registry-do.js";

const DEVICE_A = "dev_01J9Z6P8G2K4M6N8Q0RSTVWXYZ";
const DEVICE_B = "dev_01J9Z6P8G2K4M6N8Q0RSTVWXYY";
const PRINCIPAL_A = "oauth:controller-a";
const PRINCIPAL_B = "oauth:controller-b";

class FakeStorage {
  constructor(map = new Map()) { this.map = map; }
  async get(key) { return this.map.get(key); }
  async put(key, value) { this.map.set(key, structuredClone(value)); }
  async delete(key) { return this.map.delete(key); }
  async list({ prefix } = {}) { return new Map([...this.map].filter(([key]) => !prefix || key.startsWith(prefix))); }
  async transaction(fn) { return fn(this); }
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

  const mismatch = await call(registry, "herdr_mcp.work_chain.create", {
    idempotency_key: "same-key",
    portable_evidence_refs: ["ev_changed"],
  });
  assert.equal(mismatch.code, "idempotency_key_payload_mismatch");

  const otherPrincipal = await createChain(registry, "same-key", PRINCIPAL_B);
  assert.equal(otherPrincipal.ok, true);
  assert.notEqual(otherPrincipal.chain.work_chain_id, first.chain.work_chain_id);
});

test("idempotency expires after 24h and storage stays bounded without timers", async () => {
  const { registry } = makeRegistry();
  const first = await call(registry, "herdr_mcp.work_chain.create", { idempotency_key: "expiry-key" }, PRINCIPAL_A, 1000);
  const expired = await call(registry, "herdr_mcp.work_chain.create", { idempotency_key: "expiry-key" }, PRINCIPAL_A, 1000 + 24 * 60 * 60 * 1000 + 1);
  assert.equal(expired.ok, true);
  assert.equal(expired.replayed, undefined);
  assert.notEqual(expired.chain.work_chain_id, first.chain.work_chain_id);

  const bounded = makeRegistry();
  for (let index = 0; index < 512; index += 1) {
    const created = await call(bounded.registry, "herdr_mcp.work_chain.create", { idempotency_key: `bounded-${index}` }, PRINCIPAL_A, 2000 + index);
    assert.equal(created.ok, true);
  }
  const saturated = await call(bounded.registry, "herdr_mcp.work_chain.create", { idempotency_key: "bounded-overflow" }, PRINCIPAL_A, 3000);
  assert.equal(saturated.code, "idempotency_capacity_exceeded");
  const records = await bounded.storage.list({ prefix: "fleet:idempotency:v1:" });
  assert.equal(records.size, 512);
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
    repo_id: "https://github.com/whshang/herdr-mcp.git",
    base_commit: "e9281b488e093f522020db2a2c6100d92b69499f",
    branch_ref: "refs/heads/feat/lane-a",
    file_scope: ["edge/cloudflare/src"],
    runtime_scope: ["edge"],
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
