import { test } from "node:test";
import assert from "node:assert/strict";

import {
  isDeviceId,
  isRoutableDevice,
  isWorkstationId,
  newDeviceId,
  normalizeDeviceId,
  validateDeviceRecord,
} from "../dist/device-model.js";

const DEVICE = "dev_01J9Z6P8G2K4M6N8Q0RSTVWXYZ";

function record(overrides = {}) {
  return {
    device_id: DEVICE,
    workstation_id: DEVICE,
    name: "macbook-main",
    authorization: "active",
    scheduling: "enabled",
    credential_id: null,
    enrolled_at_ms: 10,
    updated_at_ms: 10,
    revoked_at_ms: null,
    ...overrides,
  };
}

test("device identity is a canonical dev_ prefixed ULID", () => {
  assert.equal(normalizeDeviceId(DEVICE.toLowerCase()), DEVICE);
  assert.equal(isDeviceId(DEVICE), true);
  assert.equal(isDeviceId("prod-real-runtime"), false);
  assert.equal(isDeviceId("dev_01J9Z6P8G2K4M6N8Q0RSTVWOYZ"), false);
});

test("new device ids use canonical ULID encoding", () => {
  const id = newDeviceId(1_788_153_600_000, new Uint8Array(10));
  assert.equal(id, "dev_01M1B456000000000000000000");
  assert.equal(isDeviceId(id), true);
  assert.throws(() => newDeviceId(1, new Uint8Array(9)), /exactly 10 bytes/);
});

test("legacy workstation ids remain valid execution identities", () => {
  assert.equal(isWorkstationId(DEVICE), true);
  assert.equal(isWorkstationId("prod-real-runtime"), true);
  assert.equal(isWorkstationId("bad/id"), false);
});

test("routability requires active authorization and enabled scheduling", () => {
  assert.equal(isRoutableDevice(record()), true);
  assert.equal(isRoutableDevice(record({ scheduling: "paused" })), false);
  assert.equal(isRoutableDevice(record({ scheduling: "draining" })), false);
  assert.equal(isRoutableDevice(record({ authorization: "suspended" })), false);
  assert.equal(isRoutableDevice(record({ authorization: "revoked", revoked_at_ms: 20 })), false);
});

test("registry record validates durable identity and desired state without realtime presence", () => {
  assert.equal(validateDeviceRecord(record()), null);
  assert.equal(validateDeviceRecord(record({ device_id: DEVICE.toLowerCase() })), "invalid_device_id");
  assert.equal(validateDeviceRecord(record({ workstation_id: "bad/id" })), "invalid_workstation_id");
  assert.equal(validateDeviceRecord(record({ authorization: "revoked" })), "missing_revoked_at");
  assert.equal(validateDeviceRecord(record({ authorization: "active", revoked_at_ms: 20 })), "unexpected_revoked_at");
  assert.equal(Object.hasOwn(record(), "connection"), false);
  assert.equal(Object.hasOwn(record(), "last_seen"), false);
});
