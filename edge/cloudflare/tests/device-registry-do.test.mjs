import { test } from "node:test";
import assert from "node:assert/strict";

import { DeviceRegistryDO } from "../dist/device-registry-do.js";

const DEVICE_A = "dev_01J9Z6P8G2K4M6N8Q0RSTVWXYZ";
const DEVICE_B = "dev_01J9Z6P8G2K4M6N8Q0RSTVWXYY";

class FakeStorage {
  constructor() {
    this.map = new Map();
    this.writeCount = 0;
  }
  async get(key) { return this.map.get(key); }
  async put(key, value) { this.writeCount += 1; this.map.set(key, structuredClone(value)); }
  async delete(key) { this.writeCount += 1; return this.map.delete(key); }
  async list({ prefix } = {}) {
    return new Map([...this.map].filter(([key]) => !prefix || key.startsWith(prefix)));
  }
  async transaction(fn) { return fn(this); }
}

function record(deviceId, overrides = {}) {
  return {
    device_id: deviceId,
    workstation_id: deviceId,
    name: deviceId === DEVICE_A ? "macbook-main" : "build-linux",
    authorization: "active",
    scheduling: "enabled",
    credential_id: null,
    enrolled_at_ms: 10,
    updated_at_ms: 10,
    revoked_at_ms: null,
    ...overrides,
  };
}

const TEST_PEPPER = "test-pepper-link-shared-secret-high-entropy-32b!!";

function makeRegistry(envOverrides = {}) {
  const storage = new FakeStorage();
  const state = { storage };
  const env = { LINK_SHARED_SECRET: TEST_PEPPER, ...envOverrides };
  return { storage, registry: new DeviceRegistryDO(state, env), env };
}

test("device registry writes durable identity records and reads them without observation writes", async () => {
  const { storage, registry } = makeRegistry();
  const put = await registry.fetch(new Request(`https://registry.internal/internal/devices/${DEVICE_A}`, {
    method: "PUT",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(record(DEVICE_A)),
  }));
  assert.equal(put.status, 200);
  assert.equal(storage.writeCount, 1);

  const beforeRead = storage.writeCount;
  const get = await registry.fetch(new Request(`https://registry.internal/internal/devices/${DEVICE_A}`));
  assert.equal(get.status, 200);
  assert.equal((await get.json()).device.device_id, DEVICE_A);

  const list = await registry.fetch(new Request("https://registry.internal/internal/devices"));
  assert.equal(list.status, 200);
  assert.deepEqual((await list.json()).devices.map((device) => device.device_id), [DEVICE_A]);
  assert.equal(storage.writeCount, beforeRead);
});

test("device registry preserves legacy workstation mapping and sorts by immutable device id", async () => {
  const { registry } = makeRegistry();
  for (const entry of [record(DEVICE_B), record(DEVICE_A, { workstation_id: "prod-real-runtime" })]) {
    const response = await registry.fetch(new Request(`https://registry.internal/internal/devices/${entry.device_id}`, {
      method: "PUT",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(entry),
    }));
    assert.equal(response.status, 200);
  }
  const response = await registry.fetch(new Request("https://registry.internal/internal/devices"));
  const body = await response.json();
  assert.deepEqual(body.devices.map((device) => device.device_id), [DEVICE_B, DEVICE_A].sort());
  assert.equal(body.devices.find((device) => device.device_id === DEVICE_A).workstation_id, "prod-real-runtime");
});

test("device registry rejects non-canonical identity and mismatched records", async () => {
  const { storage, registry } = makeRegistry();
  const lower = DEVICE_A.toLowerCase();
  const badPath = await registry.fetch(new Request(`https://registry.internal/internal/devices/${lower}`, { method: "PUT" }));
  assert.equal(badPath.status, 400);

  const mismatch = await registry.fetch(new Request(`https://registry.internal/internal/devices/${DEVICE_A}`, {
    method: "PUT",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(record(DEVICE_B)),
  }));
  assert.equal(mismatch.status, 409);
  assert.equal(storage.writeCount, 0);
});

test("legacy workstation registration creates one stable device and does not rewrite on reconnect", async () => {
  const { storage, registry } = makeRegistry();
  const makeRequest = () => new Request("https://registry.internal/internal/devices/legacy/ensure", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ workstation_id: "prod-real-runtime", name: "macbook-main" }),
  });

  const first = await registry.fetch(makeRequest());
  assert.equal(first.status, 200);
  const firstBody = await first.json();
  assert.equal(firstBody.created, true);
  assert.equal(firstBody.device.workstation_id, "prod-real-runtime");
  assert.match(firstBody.device.device_id, /^dev_[0-7][0-9A-HJKMNP-TV-Z]{25}$/);
  const writesAfterFirst = storage.writeCount;

  const second = await registry.fetch(makeRequest());
  assert.equal(second.status, 200);
  const secondBody = await second.json();
  assert.equal(secondBody.created, false);
  assert.equal(secondBody.device.device_id, firstBody.device.device_id);
  assert.equal(storage.writeCount, writesAfterFirst);
});

test("device rename updates only display metadata and repeated rename is write-free", async () => {
  const { storage, registry } = makeRegistry();
  const ensure = await registry.fetch(new Request("https://registry.internal/internal/devices/legacy/ensure", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ workstation_id: "prod-real-runtime", name: "MacBook Air" }),
  }));
  assert.equal(ensure.status, 200);
  const ensured = await ensure.json();
  const writesBeforeRename = storage.writeCount;

  const renameRequest = () => new Request("https://registry.internal/internal/devices/rename", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ workstation_id: "prod-real-runtime", name: "qingxian-macbookair" }),
  });
  const first = await registry.fetch(renameRequest());
  assert.equal(first.status, 200);
  const firstBody = await first.json();
  assert.equal(firstBody.device_id, ensured.device.device_id);
  assert.equal(firstBody.name, "qingxian-macbookair");
  assert.equal(firstBody.wrote_registry, true);
  assert.equal(storage.writeCount, writesBeforeRename + 1);

  const stored = await registry.fetch(new Request(`https://registry.internal/internal/devices/${ensured.device.device_id}`));
  const storedDevice = (await stored.json()).device;
  assert.equal(storedDevice.name, "qingxian-macbookair");
  assert.deepEqual(storedDevice.aliases, ["MacBook Air"]);
  const writesBeforeNoop = storage.writeCount;

  const second = await registry.fetch(renameRequest());
  assert.equal(second.status, 200);
  assert.equal((await second.json()).wrote_registry, false);
  assert.equal(storage.writeCount, writesBeforeNoop);
});

test("device pairing stores only digest-keyed verifiers, is single-use, and authenticates only its bound workstation", async () => {
  const { storage, registry } = makeRegistry();
  const create = await registry.fetch(new Request("https://registry.internal/internal/devices/pairings", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ ttl_seconds: 60, name: "macbook-secondary", worker_context: "herdr-edge@test" }),
  }));
  assert.equal(create.status, 200);
  const created = await create.json();
  assert.match(created.pairing_id, /^pair_[0-9a-f]{64}$/);
  assert.match(created.code, /^[0-9]{6}$/);
  assert.equal(JSON.stringify([...storage.map]).includes(created.pairing_id), false);
  assert.equal(JSON.stringify([...storage.map]).includes(created.code), false);

  const consumeRequest = () => new Request("https://registry.internal/internal/devices/pairings/consume", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ pairing_id: created.pairing_id, code: created.code, worker_context: "herdr-edge@test" }),
  });
  const consume = await registry.fetch(consumeRequest());
  assert.equal(consume.status, 200);
  const paired = await consume.json();
  assert.match(paired.device_id, /^dev_[0-7][0-9A-HJKMNP-TV-Z]{25}$/);
  assert.equal(paired.workstation_id, paired.device_id);
  assert.match(paired.credential_id, /^cred_[0-9a-f]{32}$/);
  assert.match(paired.device_secret, /^devsec_[0-9a-f]{64}$/);
  assert.equal(JSON.stringify([...storage.map]).includes(paired.device_secret), false);

  const verifier = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(paired.device_secret));
  const verifierHex = [...new Uint8Array(verifier)].map((byte) => byte.toString(16).padStart(2, "0")).join("");
  const auth = await registry.fetch(new Request("https://registry.internal/internal/devices/authenticate", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ workstation_id: paired.workstation_id, credential_verifier_sha256: verifierHex }),
  }));
  assert.equal(auth.status, 200);
  assert.equal((await auth.json()).device_id, paired.device_id);

  const replay = await registry.fetch(consumeRequest());
  assert.equal(replay.status, 401);
  assert.equal((await replay.json()).code, "pairing_rejected");
});

test("pairing defaults the display name from the joining device when the pairing creator did not override it", async () => {
  const { registry } = makeRegistry();
  const create = await registry.fetch(new Request("https://registry.internal/internal/devices/pairings", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ ttl_seconds: 60, worker_context: "herdr-edge@test" }),
  }));
  assert.equal(create.status, 200);
  const created = await create.json();

  const consume = await registry.fetch(new Request("https://registry.internal/internal/devices/pairings/consume", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      pairing_id: created.pairing_id,
      code: created.code,
      name: "青闲的 MacBook Air",
      worker_context: "herdr-edge@test",
    }),
  }));
  assert.equal(consume.status, 200);
  const paired = await consume.json();

  const stored = await registry.fetch(new Request(`https://registry.internal/internal/devices/${paired.device_id}`));
  assert.equal(stored.status, 200);
  assert.equal((await stored.json()).device.name, "青闲的 MacBook Air");
});

test("expired pairing is rejected generically and deleted", async () => {
  const { storage, registry } = makeRegistry();
  const create = await registry.fetch(new Request("https://registry.internal/internal/devices/pairings", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ ttl_seconds: 60, worker_context: "herdr-edge@test" }),
  }));
  const created = await create.json();
  const pairingEntry = [...storage.map.entries()].find(([key]) => key.startsWith("pairing:"));
  assert.ok(pairingEntry);
  storage.map.set(pairingEntry[0], { ...pairingEntry[1], expires_at_ms: Date.now() - 1 });

  const consume = await registry.fetch(new Request("https://registry.internal/internal/devices/pairings/consume", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ pairing_id: created.pairing_id, code: created.code, worker_context: "herdr-edge@test" }),
  }));
  assert.equal(consume.status, 401);
  assert.equal((await consume.json()).code, "pairing_rejected");
  assert.equal(storage.map.has(pairingEntry[0]), false);
});

test("pairing pepper is required: missing pepper fails closed on create and consume", async () => {
  const { registry: noPepperRegistry } = makeRegistry({ LINK_SHARED_SECRET: undefined, DEV_MCP_BEARER_SECRET: undefined });
  const create = await noPepperRegistry.fetch(new Request("https://registry.internal/internal/devices/pairings", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ ttl_seconds: 60, worker_context: "herdr-edge@test" }),
  }));
  assert.equal(create.status, 503);
  assert.equal((await create.json()).code, "pairing_unavailable");

  // Also consume path fails closed with uniform rejection when pepper missing
  const { storage, registry } = makeRegistry();
  const ok = await registry.fetch(new Request("https://registry.internal/internal/devices/pairings", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ ttl_seconds: 60, worker_context: "herdr-edge@test" }),
  }));
  const session = await ok.json();
  const noPepperConsumeRegistry = new DeviceRegistryDO({ storage }, {});
  const consume = await noPepperConsumeRegistry.fetch(new Request("https://registry.internal/internal/devices/pairings/consume", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ pairing_id: session.pairing_id, code: session.code, worker_context: "herdr-edge@test" }),
  }));
  assert.equal(consume.status, 401);
  assert.equal((await consume.json()).code, "pairing_rejected");
});

test("pepper never appears in DO storage, logs, or pairing output", async () => {
  const { storage, registry } = makeRegistry();
  const create = await registry.fetch(new Request("https://registry.internal/internal/devices/pairings", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ ttl_seconds: 60, worker_context: "herdr-edge@test" }),
  }));
  const session = await create.json();
  const snapshot = JSON.stringify([...storage.map]);
  assert.equal(snapshot.includes(TEST_PEPPER), false, "pepper must never be stored in DO");
  assert.equal(snapshot.includes(session.pairing_id), false);
  assert.equal(JSON.stringify(session).includes(TEST_PEPPER), false, "pepper must never be returned to clients");
});
