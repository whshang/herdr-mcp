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
  async list({ prefix } = {}) {
    return new Map([...this.map].filter(([key]) => !prefix || key.startsWith(prefix)));
  }
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
