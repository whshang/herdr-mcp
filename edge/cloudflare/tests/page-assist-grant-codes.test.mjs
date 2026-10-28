import { test } from "node:test";
import assert from "node:assert/strict";

import worker from "../dist/index.js";
import { DeviceRegistryDO } from "../dist/device-registry-do.js";

// #721: the Page Assist grant route collapsed every Durable Object failure
// into one `page_assist_grant_update_failed` code, so a mistyped connector id
// looked exactly like a revoked connector or a full grant record. These tests
// pin the specific code for each reason the Durable Object reports.

class FakeStorage {
  constructor() {
    this.map = new Map();
  }
  async get(key) { return this.map.get(key); }
  async put(key, value) { this.map.set(key, structuredClone(value)); }
  async delete(key) { return this.map.delete(key); }
  async list({ prefix } = {}) {
    return new Map([...this.map].filter(([key]) => !prefix || key.startsWith(prefix)));
  }
  async transaction(fn) { return fn(this); }
}

function namespace(stub) {
  return { idFromName: () => "devices-v1", get: () => stub };
}

// The owner authenticates through the legacy shared-secret workstation path so
// the test needs no paired device: the registry has no device for that
// workstation, which is exactly the `device_not_found` branch the route falls
// back to.
function makeEnv(oauthStub) {
  const registry = new DeviceRegistryDO(
    { storage: new FakeStorage() },
    { LINK_SHARED_SECRET: "legacy-secret", WORKSTATION_DO: namespace({ fetch: async () => new Response("{}", { status: 200 }) }) },
  );
  return {
    DEVICE_REGISTRY_DO: namespace(registry),
    WORKSTATION_DO: namespace({ fetch: async () => new Response("{}", { status: 200 }) }),
    OAUTH_STORE_DO: namespace(oauthStub),
    DEV_MCP_BEARER_SECRET: "owner-secret",
    LINK_SHARED_SECRET: "legacy-secret",
    DEFAULT_WORKSTATION_ID: "prod-real-runtime",
  };
}

function ownerRequest(body) {
  return new Request("https://edge.example/connectors/page-assist", {
    method: "POST",
    headers: {
      "content-type": "application/json",
      authorization: "Bearer legacy-secret",
      "x-herdr-workstation": "prod-real-runtime",
    },
    body: JSON.stringify(body),
  });
}

const VALID_BODY = {
  connector_id: "conn_zzzzzzzzzzzz",
  device_id: "dev_01M1BDHFE8TJ2PZGQAAG4T1A3V",
  endpoint_ref: `be_${"c".repeat(64)}`,
  allowed: true,
};

function oauthStub(code, status) {
  return {
    fetch: async () => new Response(JSON.stringify({ ok: false, code }), { status }),
  };
}

test("user learns the connector does not exist | Given a grant update the Durable Object rejects as not_found | When the route maps the failure | Then the response names the missing connector", async () => {
  const response = await worker.fetch(ownerRequest(VALID_BODY), makeEnv(oauthStub("not_found", 404)));
  assert.equal(response.status, 404);
  const body = await response.json();
  assert.equal(body.ok, false);
  assert.equal(body.code, "page_assist_grant_connector_not_found");
});

test("user learns the connector was revoked | Given a grant update the Durable Object rejects as revoked | When the route maps the failure | Then the response names the revoked grant", async () => {
  const response = await worker.fetch(ownerRequest(VALID_BODY), makeEnv(oauthStub("revoked", 409)));
  assert.equal(response.status, 409);
  const body = await response.json();
  assert.equal(body.code, "page_assist_grant_revoked");
});

test("user learns the grant record is full | Given a grant update the Durable Object rejects as capacity | When the route maps the failure | Then the response names the capacity limit", async () => {
  const response = await worker.fetch(ownerRequest(VALID_BODY), makeEnv(oauthStub("capacity", 409)));
  assert.equal(response.status, 409);
  const body = await response.json();
  assert.equal(body.code, "page_assist_grant_capacity_reached");
});

test("user learns the stored record is unreadable | Given a grant update the Durable Object rejects as invalid_record | When the route maps the failure | Then the response names the invalid record", async () => {
  const response = await worker.fetch(ownerRequest(VALID_BODY), makeEnv(oauthStub("invalid_record", 500)));
  assert.equal(response.status, 500);
  const body = await response.json();
  assert.equal(body.code, "page_assist_grant_record_invalid");
});

test("user still gets the catch-all code for an unknown failure | Given a grant update the Durable Object rejects with an unknown reason | When the route maps the failure | Then the response keeps the catch-all code", async () => {
  const response = await worker.fetch(ownerRequest(VALID_BODY), makeEnv(oauthStub("something_new", 409)));
  assert.equal(response.status, 409);
  const body = await response.json();
  assert.equal(body.code, "page_assist_grant_update_failed");
});
