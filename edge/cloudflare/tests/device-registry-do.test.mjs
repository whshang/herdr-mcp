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

function makeRegistry() {
  const storage = new FakeStorage();
  const state = { storage };
  return { storage, registry: new DeviceRegistryDO(state, {}) };
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

test("device enrollment stores only verifiers, is single-use, and authenticates only its bound workstation", async () => {
  const { storage, registry } = makeRegistry();
  const create = await registry.fetch(new Request("https://registry.internal/internal/devices/enrollments", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ ttl_seconds: 60, name: "macbook-secondary" }),
  }));
  assert.equal(create.status, 200);
  const created = await create.json();
  assert.match(created.enrollment_code, /^enroll_[0-9a-f]{64}$/);
  assert.equal(JSON.stringify([...storage.map]).includes(created.enrollment_code), false);

  const consumeRequest = () => new Request("https://registry.internal/internal/devices/enrollments/consume", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ enrollment_code: created.enrollment_code }),
  });
  const consume = await registry.fetch(consumeRequest());
  assert.equal(consume.status, 200);
  const enrolled = await consume.json();
  assert.match(enrolled.device_id, /^dev_[0-7][0-9A-HJKMNP-TV-Z]{25}$/);
  assert.equal(enrolled.workstation_id, enrolled.device_id);
  assert.match(enrolled.credential_id, /^cred_[0-9a-f]{32}$/);
  assert.match(enrolled.device_secret, /^devsec_[0-9a-f]{64}$/);
  assert.equal(JSON.stringify([...storage.map]).includes(enrolled.device_secret), false);

  const verifier = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(enrolled.device_secret));
  const verifierHex = [...new Uint8Array(verifier)].map((byte) => byte.toString(16).padStart(2, "0")).join("");
  const auth = await registry.fetch(new Request("https://registry.internal/internal/devices/authenticate", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ workstation_id: enrolled.workstation_id, credential_verifier_sha256: verifierHex }),
  }));
  assert.equal(auth.status, 200);
  assert.equal((await auth.json()).device_id, enrolled.device_id);

  const replay = await registry.fetch(consumeRequest());
  assert.equal(replay.status, 401);
  assert.equal((await replay.json()).code, "invalid_enrollment");
});

test("expired enrollment is rejected and deleted", async () => {
  const { storage, registry } = makeRegistry();
  const create = await registry.fetch(new Request("https://registry.internal/internal/devices/enrollments", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ ttl_seconds: 60 }),
  }));
  const created = await create.json();
  const enrollmentEntry = [...storage.map.entries()].find(([key]) => key.startsWith("enrollment:"));
  assert.ok(enrollmentEntry);
  enrollmentEntry[1].expires_at_ms = Date.now() - 1;
  storage.map.set(enrollmentEntry[0], enrollmentEntry[1]);

  const consume = await registry.fetch(new Request("https://registry.internal/internal/devices/enrollments/consume", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ enrollment_code: created.enrollment_code }),
  }));
  assert.equal(consume.status, 410);
  assert.equal((await consume.json()).code, "enrollment_expired");
  assert.equal(storage.map.has(enrollmentEntry[0]), false);
});
