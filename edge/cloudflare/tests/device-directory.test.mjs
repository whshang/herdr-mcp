import { test } from "node:test";
import assert from "node:assert/strict";

import {
  ensureLegacyDeviceRegistration,
  listPublicDevices,
  resolveDeviceRoute,
} from "../dist/device-directory.js";

const DEV_A = "dev_01J9Z6P8G2K4M6N8Q0RSTVWXYZ";
const DEV_B = "dev_01J9Z6P8G2K4M6N8Q0RSTVWXYY";

function response(body, status = 200) {
  return new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
}

function registryWith(devices) {
  return { fetch: async () => response({ ok: true, devices }) };
}

test("device directory joins durable identity with read-only WorkstationDO state", async () => {
  const registry = {
    fetch: async () => response({ ok: true, devices: [
      { device_id: DEV_A, workstation_id: "prod-real-runtime", name: "macbook", authorization: "active", scheduling: "enabled" },
      { device_id: DEV_B, workstation_id: DEV_B, name: "build-linux", authorization: "active", scheduling: "paused" },
    ] }),
  };
  const statuses = new Map([
    ["prod-real-runtime", { online: true, connected: true, runtimeHealth: "healthy", runtimeVersion: "0.4.2", runtimeGeneration: "rust-a", lastSeenAgoMs: 12, activeRequests: 2 }],
    [DEV_B, { online: false, connected: true, runtimeHealth: "healthy", runtimeVersion: "0.4.2", activeRequests: 0 }],
  ]);
  const devices = await listPublicDevices(registry, (id) => ({ fetch: async () => response(statuses.get(id)) }));
  assert.equal(devices.length, 2);
  assert.equal(devices[0].device_id, DEV_A);
  assert.equal(devices[0].connection, "online");
  assert.equal(devices[0].runtime_generation, "rust-a");
  assert.equal(devices[1].connection, "stale");
  assert.equal(Object.hasOwn(devices[0], "workstation_id"), false);
  assert.equal(Object.hasOwn(devices[0], "credential_id"), false);
});

test("device directory degrades one failed realtime status to offline", async () => {
  const registry = {
    fetch: async () => response({ ok: true, devices: [
      { device_id: DEV_A, workstation_id: DEV_A, name: "macbook", authorization: "active", scheduling: "enabled" },
    ] }),
  };
  const devices = await listPublicDevices(registry, () => ({ fetch: async () => response({ ok: false }, 503) }));
  assert.equal(devices[0].device_id, DEV_A);
  assert.equal(devices[0].connection, "offline");
  assert.equal(devices[0].health, "unknown");
});

test("legacy registration helper uses the registry mutation endpoint without exposing credentials", async () => {
  let captured = null;
  const registry = {
    fetch: async (request) => {
      captured = { url: request.url, body: await request.json() };
      return response({ ok: true, created: true, device: { device_id: DEV_A } });
    },
  };
  const result = await ensureLegacyDeviceRegistration(registry, "prod-real-runtime");
  assert.deepEqual(result, { device_id: DEV_A, created: true });
  assert.equal(new URL(captured.url).pathname, "/internal/devices/legacy/ensure");
  assert.deepEqual(captured.body, { workstation_id: "prod-real-runtime", name: "prod-real-runtime" });
  assert.equal(Object.keys(captured.body).some((key) => /secret|token|credential/i.test(key)), false);
});

test("device routing prefers explicit immutable identity and resolves unique aliases", async () => {
  const devices = [
    { device_id: DEV_A, workstation_id: "prod-real-runtime", name: "qingxian-macbookair", aliases: ["macbook"], authorization: "active", scheduling: "enabled" },
    { device_id: DEV_B, workstation_id: DEV_B, name: "build-linux", authorization: "active", scheduling: "enabled" },
  ];
  assert.deepEqual(await resolveDeviceRoute(registryWith(devices), DEV_B.toLowerCase(), "legacy"), {
    ok: true,
    device_id: DEV_B,
    device_name: "build-linux",
    workstation_id: DEV_B,
    routing_reason: "explicit_device",
  });
  assert.deepEqual(await resolveDeviceRoute(registryWith(devices), "macbook", "legacy"), {
    ok: true,
    device_id: DEV_A,
    device_name: "qingxian-macbookair",
    workstation_id: "prod-real-runtime",
    routing_reason: "explicit_device",
  });
  assert.deepEqual(await resolveDeviceRoute(registryWith(devices), "qingxian-macbookair", "legacy"), {
    ok: true,
    device_id: DEV_A,
    device_name: "qingxian-macbookair",
    workstation_id: "prod-real-runtime",
    routing_reason: "explicit_device",
  });
});

test("device routing fails closed for ambiguity and paused/suspended devices", async () => {
  const duplicateNames = [
    { device_id: DEV_A, workstation_id: DEV_A, name: "build", authorization: "active", scheduling: "enabled" },
    { device_id: DEV_B, workstation_id: DEV_B, name: "build", authorization: "active", scheduling: "enabled" },
  ];
  assert.deepEqual(await resolveDeviceRoute(registryWith(duplicateNames), undefined, "legacy"), {
    ok: false,
    code: "device_ambiguous",
    candidate_devices: [
      { device_id: DEV_A, name: "build" },
      { device_id: DEV_B, name: "build" },
    ],
  });
  assert.deepEqual(await resolveDeviceRoute(registryWith(duplicateNames), "build", "legacy"), {
    ok: false,
    code: "device_ambiguous",
    candidate_devices: [
      { device_id: DEV_A, name: "build" },
      { device_id: DEV_B, name: "build" },
    ],
  });

  const paused = [{ ...duplicateNames[0], scheduling: "paused" }];
  assert.deepEqual(await resolveDeviceRoute(registryWith(paused), DEV_A, "legacy"), {
    ok: false,
    code: "device_paused",
    selected_device: { device_id: DEV_A, name: "build" },
  });
  const suspended = [{ ...duplicateNames[0], authorization: "suspended" }];
  assert.deepEqual(await resolveDeviceRoute(registryWith(suspended), DEV_A, "legacy"), {
    ok: false,
    code: "device_suspended",
    selected_device: { device_id: DEV_A, name: "build" },
  });
});

test("implicit routing preserves the legacy default workstation after additional devices enroll", async () => {
  const devices = [
    { device_id: DEV_A, workstation_id: "prod-real-runtime", name: "primary", authorization: "active", scheduling: "enabled" },
    { device_id: DEV_B, workstation_id: DEV_B, name: "secondary", authorization: "active", scheduling: "enabled" },
  ];
  assert.deepEqual(await resolveDeviceRoute(registryWith(devices), undefined, "prod-real-runtime"), {
    ok: true,
    device_id: DEV_A,
    device_name: "primary",
    workstation_id: "prod-real-runtime",
    routing_reason: "legacy_default_device",
  });

  const pausedDefault = [{ ...devices[0], scheduling: "paused" }, devices[1]];
  assert.deepEqual(await resolveDeviceRoute(registryWith(pausedDefault), undefined, "prod-real-runtime"), {
    ok: false,
    code: "device_paused",
    selected_device: { device_id: DEV_A, name: "primary" },
  });

  const noLegacyMatch = [
    { ...devices[0], workstation_id: "ws-a" },
    { ...devices[1], workstation_id: "ws-b" },
  ];
  assert.deepEqual(await resolveDeviceRoute(registryWith(noLegacyMatch), undefined, "legacy"), {
    ok: false,
    code: "device_ambiguous",
    candidate_devices: [
      { device_id: DEV_A, name: "primary" },
      { device_id: DEV_B, name: "secondary" },
    ],
  });
});

test("device routing selects one routable device and preserves legacy fallback only for an empty registry", async () => {
  const single = [{ device_id: DEV_A, workstation_id: "prod-real-runtime", name: "macbook", authorization: "active", scheduling: "enabled" }];
  assert.deepEqual(await resolveDeviceRoute(registryWith(single), undefined, "legacy"), {
    ok: true,
    device_id: DEV_A,
    device_name: "macbook",
    workstation_id: "prod-real-runtime",
    routing_reason: "single_available_device",
  });
  assert.deepEqual(await resolveDeviceRoute(registryWith([]), undefined, "prod-real-runtime"), {
    ok: true,
    device_id: null,
    workstation_id: "prod-real-runtime",
    routing_reason: "legacy_default_device",
  });
});
