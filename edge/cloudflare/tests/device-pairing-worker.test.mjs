import { test } from "node:test";
import assert from "node:assert/strict";

import worker from "../dist/index.js";
import { buildLinkAuthProtocol } from "../dist/auth.js";
import { DeviceRegistryDO } from "../dist/device-registry-do.js";
import { OAuthStoreDO } from "../dist/oauth-store-do.js";

// Transaction-capable fake: concurrent transactions are serialized like a real
// Durable Object storage transaction, so the race test exercises the same
// atomicity guarantees as production.
class FakeStorage {
  constructor() {
    this.map = new Map();
    this.writeCount = 0;
    this._queue = Promise.resolve();
  }
  async get(key) { return this.map.get(key); }
  async put(key, value) { this.writeCount += 1; this.map.set(key, structuredClone(value)); }
  async delete(key) { this.writeCount += 1; return this.map.delete(key); }
  async list({ prefix } = {}) {
    return new Map([...this.map].filter(([key]) => !prefix || key.startsWith(prefix)));
  }
  transaction(fn) {
    const run = this._queue.then(() => fn(this));
    this._queue = run.then(() => undefined, () => undefined);
    return run;
  }
  async getAlarm() { return null; }
  async setAlarm() { throw new Error("pairing flow must not schedule alarms"); }
  async deleteAlarm() {}
}

function namespace(stub) {
  return { idFromName: () => "devices-v1", get: () => stub };
}

function post(path, body, authorization) {
  const headers = { "content-type": "application/json" };
  if (authorization) headers.authorization = `Bearer ${authorization}`;
  return new Request(`https://edge.example${path}`, { method: "POST", headers, body: JSON.stringify(body) });
}

function postAsWorkstation(path, body, workstationId, secret) {
  return new Request(`https://edge.example${path}`, {
    method: "POST",
    headers: {
      "content-type": "application/json",
      authorization: `Bearer ${secret}`,
      "x-herdr-workstation": workstationId,
    },
    body: JSON.stringify(body),
  });
}

function get(path, authorization, workstationId = null) {
  const headers = {};
  if (authorization) headers.authorization = `Bearer ${authorization}`;
  if (workstationId) headers["x-herdr-workstation"] = workstationId;
  return new Request(`https://edge.example${path}`, { method: "GET", headers });
}

// Finite POST /mcp initialize exchange — never a streaming GET /mcp SSE probe.
function mcpInitialize(authorization, id = 1) {
  const headers = { "content-type": "application/json" };
  if (authorization) headers.authorization = `Bearer ${authorization}`;
  return new Request(`https://edge.example/mcp`, {
    method: "POST",
    headers,
    body: JSON.stringify({ jsonrpc: "2.0", id, method: "initialize", params: {} }),
  });
}

function base64UrlUtf8(value) {
  const bytes = new TextEncoder().encode(value);
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/g, "");
}

async function legacyPemJwt(clientId, issuer = "https://edge.example") {
  const keys = await crypto.subtle.generateKey(
    { name: "RSASSA-PKCS1-v1_5", modulusLength: 2048, publicExponent: new Uint8Array([1, 0, 1]), hash: "SHA-256" },
    true,
    ["sign", "verify"],
  );
  const der = new Uint8Array(await crypto.subtle.exportKey("spki", keys.publicKey));
  const chunks = Buffer.from(der).toString("base64").match(/.{1,64}/g) ?? [];
  const pem = `-----BEGIN PUBLIC KEY-----\n${chunks.join("\n")}\n-----END PUBLIC KEY-----\n`;
  const now = Math.floor(Date.now() / 1000);
  const enc = new TextEncoder();
  const header = Buffer.from(JSON.stringify({ alg: "RS256", typ: "at+jwt" })).toString("base64url");
  const payload = Buffer.from(JSON.stringify({
    iss: issuer,
    aud: `${issuer}/mcp`,
    sub: clientId,
    client_id: clientId,
    iat: now,
    exp: now + 3600,
  })).toString("base64url");
  const signing = `${header}.${payload}`;
  const signature = await crypto.subtle.sign("RSASSA-PKCS1-v1_5", keys.privateKey, enc.encode(signing));
  return { pem, token: `${signing}.${Buffer.from(signature).toString("base64url")}` };
}

function wsRequest(workstationId, secret, deviceName) {
  const headers = {
    Upgrade: "websocket",
    "sec-websocket-protocol": `herdr-link.v1, ${buildLinkAuthProtocol(secret)}`,
  };
  if (deviceName) headers["x-herdr-device-name-b64"] = base64UrlUtf8(deviceName);
  return new Request(`https://edge.example/ws/${encodeURIComponent(workstationId)}`, {
    method: "GET",
    headers,
  });
}

function makeEnv(extra = {}) {
  const storage = new FakeStorage();
  const forwarded = [];
  const workstationStub = {
    async fetch(request) {
      forwarded.push(new URL(request.url).pathname);
      return new Response(JSON.stringify({ ok: true }), { status: 200 });
    },
  };
  const registry = new DeviceRegistryDO(
    { storage },
    { LINK_SHARED_SECRET: "legacy-secret", WORKSTATION_DO: namespace(workstationStub) },
  );
  const unusedOAuth = { fetch: async () => new Response(JSON.stringify({ ok: false }), { status: 401 }) };
  return {
    storage,
    forwarded,
    registry,
    env: {
      DEVICE_REGISTRY_DO: namespace(registry),
      WORKSTATION_DO: namespace(workstationStub),
      OAUTH_STORE_DO: namespace(unusedOAuth),
      DEV_MCP_BEARER_SECRET: "owner-secret",
      LINK_SHARED_SECRET: "legacy-secret",
      DEFAULT_WORKSTATION_ID: "prod-real-runtime",
      ...extra,
    },
  };
}

async function pair(env, name) {
  const create = await worker.fetch(post("/devices/pairings", { ttl_seconds: 60, name }, "owner-secret"), env);
  assert.equal(create.status, 200);
  assert.equal(create.headers.get("cache-control"), "no-store");
  const session = await create.json();
  const consume = await worker.fetch(post("/devices/pairings/consume", { pairing_id: session.pairing_id, code: session.code }), env);
  assert.equal(consume.status, 200);
  assert.equal(consume.headers.get("cache-control"), "no-store");
  return consume.json();
}

test("legacy public-PEM JWT keeps ordinary compatibility until its client grant is revoked", async () => {
  const h = makeEnv({ OAUTH_ISSUER: "https://edge.example" });
  const oauthStorage = new FakeStorage();
  const oauth = new OAuthStoreDO({ storage: oauthStorage }, { OAUTH_ISSUER: "https://edge.example" });
  h.env.OAUTH_STORE_DO = namespace(oauth);
  const clientId = "legacy-pem-client";
  const legacy = await legacyPemJwt(clientId);
  h.env.OAUTH_JWT_PUBLIC_PEM = legacy.pem;

  const before = await worker.fetch(mcpInitialize(legacy.token), h.env);
  assert.equal(before.status, 200, "pre-v0.4.6 JWT without a grant record keeps ordinary MCP compatibility");
  const beforeBody = await before.json();
  assert.equal(beforeBody.result.serverInfo.name, "herdr-mcp");

  const revoked = await oauth.fetch(new Request("https://oauth.internal/internal/oauth/grant/revoke", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ client_id: clientId, revoked_by: "device:test", now_ms: Date.now() }),
  }));
  assert.equal(revoked.status, 200);

  const after = await worker.fetch(mcpInitialize(legacy.token, 2), h.env);
  assert.equal(after.status, 401, "legacy PEM JWT is fenced immediately by the revoke tombstone");
});

test("fleet admin provisions independently revocable Automation Client bound to a device for CI without granting fleet-admin", async () => {
  const h = makeEnv();
  const oauthStorage = new FakeStorage();
  const oauth = new OAuthStoreDO({ storage: oauthStorage }, { OAUTH_ISSUER: "https://edge.example" });
  h.env.OAUTH_STORE_DO = namespace(oauth);
  h.env.OAUTH_ISSUER = "https://edge.example";

  // An Automation Client must be bound to exactly one enrolled device.
  const boundDevice = await pair(h.env, "ci-bound-mac");
  const deviceSelector = boundDevice.device_id; // immutable dev_<26> identity

  const missingDevice = await worker.fetch(
    post("/automations", { name: "gitlab:group/project:prod" }, "owner-secret"),
    h.env,
  );
  assert.equal(missingDevice.status, 400);
  assert.equal((await missingDevice.json()).code, "bad_request");

  const createdResponse = await worker.fetch(
    post("/automations", { name: "gitlab:group/project:prod", device: deviceSelector }, "owner-secret"),
    h.env,
  );
  assert.equal(createdResponse.status, 201);
  const created = await createdResponse.json();
  assert.match(created.client_id, /^svc_[A-Za-z0-9_-]+$/);
  assert.match(created.client_secret, /^herdr_svc_/);
  assert.equal(created.scope, "mcp");
  assert.equal(created.device_id, deviceSelector);
  assert.equal(created.device_name, "ci-bound-mac");

  const listResponse = await worker.fetch(get("/automations", "owner-secret"), h.env);
  assert.equal(listResponse.status, 200);
  const listed = await listResponse.json();
  assert.equal(listed.automations.length, 1);
  assert.equal(listed.automations[0].client_id, created.client_id);
  assert.equal(listed.automations[0].name, "gitlab:group/project:prod");
  assert.equal(listed.automations[0].device_id, deviceSelector);
  assert.equal(listed.automations[0].client_secret, undefined);

  const tokenResponse = await worker.fetch(new Request("https://edge.example/oauth/token", {
    method: "POST",
    headers: { "content-type": "application/x-www-form-urlencoded" },
    body: new URLSearchParams({
      grant_type: "client_credentials",
      client_id: created.client_id,
      client_secret: created.client_secret,
      resource: "https://edge.example/mcp",
    }),
  }), h.env);
  assert.equal(tokenResponse.status, 200);
  const token = await tokenResponse.json();
  assert.equal(token.expires_in, 3600);
  assert.equal(token.refresh_token, undefined);

  const adminDenied = await worker.fetch(
    post("/devices/pairings", { ttl_seconds: 60 }, token.access_token),
    h.env,
  );
  assert.equal(adminDenied.status, 401);
  assert.equal((await adminDenied.json()).code, "pairing_admin_required");

  const rotatedResponse = await worker.fetch(
    post("/automations/rotate", { client_id: created.client_id }, "owner-secret"),
    h.env,
  );
  assert.equal(rotatedResponse.status, 200);
  const rotated = await rotatedResponse.json();
  assert.match(rotated.client_secret, /^herdr_svc_/);
  assert.notEqual(rotated.client_secret, created.client_secret);

  const oldSecret = await worker.fetch(new Request("https://edge.example/oauth/token", {
    method: "POST",
    headers: { "content-type": "application/x-www-form-urlencoded" },
    body: new URLSearchParams({
      grant_type: "client_credentials",
      client_id: created.client_id,
      client_secret: created.client_secret,
      resource: "https://edge.example/mcp",
    }),
  }), h.env);
  assert.equal(oldSecret.status, 400);
  assert.equal((await oldSecret.json()).error, "invalid_client");

  const revokedResponse = await worker.fetch(
    post("/automations/revoke", { client_id: created.client_id }, "owner-secret"),
    h.env,
  );
  assert.equal(revokedResponse.status, 200);
  assert.equal((await revokedResponse.json()).action, "automation_revoke");

  const existingAccessAfterRevoke = await worker.fetch(new Request("https://edge.example/mcp", {
    method: "POST",
    headers: {
      "content-type": "application/json",
      authorization: `Bearer ${token.access_token}`,
    },
    body: JSON.stringify({ jsonrpc: "2.0", id: 991, method: "tools/list", params: {} }),
  }), h.env);
  assert.equal(existingAccessAfterRevoke.status, 401);
});

test("automation token routes only to its bound device and cannot discover/route to another", async () => {
  const h = makeEnv();
  const oauthStorage = new FakeStorage();
  const oauth = new OAuthStoreDO({ storage: oauthStorage }, { OAUTH_ISSUER: "https://edge.example" });
  h.env.OAUTH_STORE_DO = namespace(oauth);
  h.env.OAUTH_ISSUER = "https://edge.example";

  const boundDevice = await pair(h.env, "auto-bound");
  const otherDevice = await pair(h.env, "auto-other");
  const created = await (await worker.fetch(
    post("/automations", { name: "gitlab:p:prod", device: boundDevice.device_id }, "owner-secret"),
    h.env,
  )).json();

  const mintToken = async (secret) => {
    const resp = await worker.fetch(new Request("https://edge.example/oauth/token", {
      method: "POST",
      headers: { "content-type": "application/x-www-form-urlencoded" },
      body: new URLSearchParams({
        grant_type: "client_credentials",
        client_id: created.client_id,
        client_secret: secret,
        resource: "https://edge.example/mcp",
      }),
    }), h.env);
    assert.equal(resp.status, 200);
    return (await resp.json()).access_token;
  };
  const token = await mintToken(created.client_secret);

  const call = async (args) => {
    const resp = await worker.fetch(new Request("https://edge.example/mcp", {
      method: "POST",
      headers: { "content-type": "application/json", authorization: `Bearer ${token}` },
      body: JSON.stringify({ jsonrpc: "2.0", id: 50, method: "tools/call", params: { name: "herdr_call", arguments: args } }),
    }), h.env);
    return resp.json();
  };

  // Explicit use of the bound device remains allowed. Omitted-device default
  // routing is covered at the handler boundary in mcp-handler.test.mjs.
  const forwardedBeforeBound = h.forwarded.length;
  await call({ method: "herdr_mcp.text.read", device: boundDevice.device_id });
  assert.equal(
    h.forwarded.length,
    forwardedBeforeBound + 1,
    "routing to the bound device reaches the workstation exactly once",
  );

  // Explicitly selecting another device fails closed with a stable error.
  const other = await call({ method: "herdr_mcp.text.read", device: otherDevice.device_id });
  assert.equal(other.result.isError, true);
  assert.equal(other.result.structuredContent.code, "automation_device_scope_violation");
  assert.equal(other.result.structuredContent.delivery_state, "not_delivered");
  assert.equal(h.forwarded.length, forwardedBeforeBound + 1, "cross-device selection must not forward");

  // herdr_devices is filtered to the bound device only.
  const listResponse = await worker.fetch(new Request("https://edge.example/mcp", {
    method: "POST",
    headers: { "content-type": "application/json", authorization: `Bearer ${token}` },
    body: JSON.stringify({ jsonrpc: "2.0", id: 51, method: "tools/call", params: { name: "herdr_devices", arguments: {} } }),
  }), h.env);
  const list = await listResponse.json();
  assert.equal(list.result.structuredContent.scope, "bound_device");
  assert.equal(list.result.structuredContent.bound_device_id, boundDevice.device_id);
  assert.equal(list.result.structuredContent.devices.length, 1);
  assert.equal(list.result.structuredContent.devices[0].device_id, boundDevice.device_id);
  assert.equal(
    list.result.structuredContent.devices.some((d) => d.device_id === otherDevice.device_id),
    false,
    "automation must never discover another device via herdr_devices",
  );

  // Automation cannot pair or revoke devices or approve Connectors.
  const pairDenied = await call({ method: "herdr_mcp.device.pair" });
  assert.equal(pairDenied.result.structuredContent.code, "fleet_admin_required");
  const revoke = await call({ method: "herdr_mcp.device.revoke", params: { device_id: otherDevice.device_id, confirm: true } });
  assert.equal(revoke.result.structuredContent.code, "fleet_admin_required");
  const approve = await call({ method: "herdr_mcp.connector.approve", params: { request_id: "r", code: "123456" } });
  assert.equal(approve.result.structuredContent.code, "fleet_admin_required");

  // Revoking the grant immediately fences the already-issued automation JWT.
  const revokeResp = await worker.fetch(
    post("/automations/revoke", { client_id: created.client_id }, "owner-secret"),
    h.env,
  );
  assert.equal(revokeResp.status, 200);
  const after = await worker.fetch(new Request("https://edge.example/mcp", {
    method: "POST",
    headers: { "content-type": "application/json", authorization: `Bearer ${token}` },
    body: JSON.stringify({ jsonrpc: "2.0", id: 52, method: "initialize", params: {} }),
  }), h.env);
  assert.equal(after.status, 401, "revoked automation grant fences an already-issued access JWT");
});

test("new Connector requires Worker fleet-admin approval and operator credentials may approve", async () => {
  const h = makeEnv();
  const oauthStorage = new FakeStorage();
  const oauth = new OAuthStoreDO({ storage: oauthStorage }, { OAUTH_ISSUER: "https://edge.example" });
  h.env.OAUTH_STORE_DO = namespace(oauth);
  h.env.OAUTH_ISSUER = "https://edge.example";

  const registration = await worker.fetch(new Request("https://edge.example/oauth/register", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      redirect_uris: ["https://client.example/callback"],
      token_endpoint_auth_method: "none",
      client_name: "Test Connector",
    }),
  }), h.env);
  assert.equal(registration.status, 201);
  const client = await registration.json();

  const authorize = new URL("https://edge.example/oauth/authorize");
  authorize.searchParams.set("client_id", client.client_id);
  authorize.searchParams.set("redirect_uri", "https://client.example/callback");
  authorize.searchParams.set("response_type", "code");
  authorize.searchParams.set("code_challenge", "A".repeat(43));
  authorize.searchParams.set("code_challenge_method", "S256");
  authorize.searchParams.set("state", "state-connector");
  const pendingResponse = await worker.fetch(new Request(authorize), h.env);
  assert.equal(pendingResponse.status, 200);
  const html = await pendingResponse.text();
  const requestId = /const requestId="([A-Za-z0-9_-]+)";/.exec(html)?.[1];
  const resumeToken = /const resumeToken="([A-Za-z0-9_-]+)";/.exec(html)?.[1];
  const approvalCode = /<div class="approval-code">(\d{6})<\/div>/.exec(html)?.[1];
  assert.ok(requestId && resumeToken && approvalCode);

  const unauthInspect = await worker.fetch(post("/connectors/inspect", { request_id: requestId }), h.env);
  assert.equal(unauthInspect.status, 401);
  const inspect = await worker.fetch(post("/connectors/inspect", { request_id: requestId }, "owner-secret"), h.env);
  assert.equal(inspect.status, 200);
  const details = await inspect.json();
  assert.equal(details.client_id, client.client_id);
  assert.equal(details.client_name, "Test Connector");
  assert.equal(details.redirect_uri, "https://client.example/callback");
  assert.equal(details.scope, "mcp");

  const approved = await worker.fetch(post("/connectors/approve", { request_id: requestId, code: approvalCode }, "owner-secret"), h.env);
  assert.equal(approved.status, 200);
  assert.equal((await approved.json()).client_id, client.client_id);

  const poll = new URL("https://edge.example/oauth/authorize/poll");
  poll.searchParams.set("request_id", requestId);
  poll.searchParams.set("resume_token", resumeToken);
  const settled = await worker.fetch(new Request(poll), h.env);
  assert.equal(settled.status, 200);
  const settledBody = await settled.json();
  assert.equal(settledBody.status, "approved");
  const redirect = new URL(settledBody.redirect);
  assert.equal(redirect.origin + redirect.pathname, "https://client.example/callback");
  assert.equal(redirect.searchParams.get("state"), "state-connector");
  assert.ok(redirect.searchParams.get("code"));

  const nowSec = Math.floor(Date.now() / 1000);
  await oauthStorage.put("client:https://legacy.example/oauth/client-metadata.json", {
    client_secret_hash: "must-never-be-returned",
    redirect_uris: ["https://legacy.example/callback"],
    token_endpoint_auth_method: "client_secret_post",
    grant_types: ["authorization_code", "refresh_token"],
    scope: "mcp",
    client_name: "Legacy WebChat",
    issued_at: nowSec - 60,
  });
  await oauthStorage.put("access:legacy-test", {
    client_id: "https://legacy.example/oauth/client-metadata.json",
    resource: "https://edge.example/mcp",
    scope: "mcp",
    expires_at: nowSec + 3600,
  });
  await oauthStorage.put("refresh:legacy-test", {
    client_id: "https://legacy.example/oauth/client-metadata.json",
    resource: "https://edge.example/mcp",
    scope: "mcp",
    expires_at: nowSec + 7200,
  });

  const inventory = await worker.fetch(new Request("https://edge.example/connectors", {
    headers: { authorization: "Bearer owner-secret" },
  }), h.env);
  assert.equal(inventory.status, 200);
  const inventoryBody = await inventory.json();
  assert.equal(inventoryBody.connectors.length, 1);
  const connectorId = inventoryBody.connectors[0].connector_id;
  assert.match(connectorId, /^conn_[A-Za-z0-9_-]+$/);
  assert.equal(inventoryBody.legacy_clients.length, 1);
  assert.deepEqual(inventoryBody.legacy_clients[0], {
    client_id: "https://legacy.example/oauth/client-metadata.json",
    client_name: "Legacy WebChat",
    issued_at: nowSec - 60,
    grant_status: null,
    registration_state: "active_credentials",
    active_access_tokens: 1,
    active_refresh_tokens: 1,
  });
  assert.equal(inventoryBody.legacy_clients[0].client_secret_hash, undefined);
  assert.equal(inventoryBody.token_counts.active_access, 1);
  assert.equal(inventoryBody.token_counts.active_refresh, 1);

  const revokeInstance = await worker.fetch(
    post("/connectors/revoke", { connector_id: connectorId }, "owner-secret"),
    h.env,
  );
  assert.equal(revokeInstance.status, 200);
  assert.equal((await revokeInstance.json()).connector_id, connectorId);

  const killUnknownLegacy = await worker.fetch(
    post("/connectors/revoke", { client_id: "legacy-client-without-store-record" }, "owner-secret"),
    h.env,
  );
  assert.equal(killUnknownLegacy.status, 200);
  assert.equal((await killUnknownLegacy.json()).client_id, "legacy-client-without-store-record");
});

test("MCP Connector administration requires fleet-admin; approved Connectors are ordinary principals", async () => {
  // Even an explicitly active grant for a Connector must NOT be able to
  // approve/revoke other Connectors. Only an enrolled Device / operator
  // (dev_bearer / static_bearer / device credential) may administer Connectors.
  const activeConnectorStub = {
    async fetch(request) {
      const url = new URL(request.url);
      const input = await request.json();
      if (url.pathname === "/internal/oauth/access/verify" && input.token === "oauth-admin-token") {
        return new Response(JSON.stringify({ ok: true, client_id: "admin-connector" }), {
          status: 200,
          headers: { "content-type": "application/json" },
        });
      }
      if (url.pathname === "/internal/oauth/grant/get" && input.client_id === "admin-connector") {
        return new Response(JSON.stringify({
          ok: true,
          record: {
            client_id: "admin-connector",
            resource: "https://edge.example/mcp",
            scope: "mcp",
            status: "active",
            approved_at_ms: 1,
            approved_by: "device:dev_01M1TEST000000000000000000",
          },
        }), { status: 200, headers: { "content-type": "application/json" } });
      }
      if (url.pathname === "/internal/oauth/approval/approve" && input.request_id === "req-child") {
        throw new Error("approved Connector must never reach the Connector-approval store");
      }
      return new Response(JSON.stringify({ ok: false, code: "not_found" }), {
        status: 404,
        headers: { "content-type": "application/json" },
      });
    },
  };
  const h = makeEnv({ OAUTH_STORE_DO: namespace(activeConnectorStub), OAUTH_ISSUER: "https://edge.example" });
  const response = await worker.fetch(new Request("https://edge.example/mcp", {
    method: "POST",
    headers: { "content-type": "application/json", authorization: "Bearer oauth-admin-token" },
    body: JSON.stringify({
      jsonrpc: "2.0",
      id: 31,
      method: "tools/call",
      params: {
        name: "herdr_call",
        arguments: {
          method: "herdr_mcp.connector.approve",
          params: { request_id: "req-child", code: "123456" },
        },
      },
    }),
  }), h.env);
  assert.equal(response.status, 200);
  const body = await response.json();
  assert.equal(body.result.isError, true);
  assert.equal(body.result.structuredContent.code, "fleet_admin_required");

  const legacyNoGrantStub = {
    async fetch(request) {
      const url = new URL(request.url);
      const input = await request.json();
      if (url.pathname === "/internal/oauth/access/verify" && input.token === "oauth-legacy-token") {
        return new Response(JSON.stringify({ ok: true, client_id: "legacy-connector" }), {
          status: 200,
          headers: { "content-type": "application/json" },
        });
      }
      if (url.pathname === "/internal/oauth/grant/get" && input.client_id === "legacy-connector") {
        return new Response(JSON.stringify({ ok: false, code: "not_found" }), {
          status: 404,
          headers: { "content-type": "application/json" },
        });
      }
      throw new Error(`unexpected OAuth store call ${url.pathname}`);
    },
  };
  const deniedEnv = makeEnv({ OAUTH_STORE_DO: namespace(legacyNoGrantStub), OAUTH_ISSUER: "https://edge.example" });
  const denied = await worker.fetch(new Request("https://edge.example/mcp", {
    method: "POST",
    headers: { "content-type": "application/json", authorization: "Bearer oauth-legacy-token" },
    body: JSON.stringify({
      jsonrpc: "2.0",
      id: 32,
      method: "tools/call",
      params: {
        name: "herdr_call",
        arguments: {
          method: "herdr_mcp.connector.approve",
          params: { request_id: "req-grandchild", code: "654321" },
        },
      },
    }),
  }), deniedEnv.env);
  assert.equal(denied.status, 200);
  const deniedBody = await denied.json();
  assert.equal(deniedBody.result.isError, true);
  assert.equal(deniedBody.result.structuredContent.code, "fleet_admin_required");
});

test("pairing creation requires fleet-admin auth and returns one-time material with worker origin metadata", async () => {
  const { env, storage } = makeEnv();
  const denied = await worker.fetch(post("/devices/pairings", { ttl_seconds: 60 }), env);
  assert.equal(denied.status, 401);
  assert.equal((await denied.json()).code, "pairing_admin_required");

  const create = await worker.fetch(post("/devices/pairings", { ttl_seconds: 60, name: "mac-a" }, "owner-secret"), env);
  assert.equal(create.status, 200);
  const body = await create.json();
  assert.match(body.pairing_id, /^pair_[0-9a-f]{64}$/);
  assert.match(body.code, /^[0-9]{6}$/);
  assert.equal(typeof body.expires_at_ms, "number");
  assert.equal(body.worker_origin, "https://edge.example");
  assert.equal(body.device_secret, undefined, "no final device secret exists before consumption");

  const snapshot = JSON.stringify([...storage.map]);
  assert.equal(snapshot.includes(body.pairing_id), false, "raw pairing_id must never be stored");
  assert.equal(snapshot.includes(body.code), false, "raw code must never be stored");
});

test("pairing optional name accepts omission, rejects null, and preserves explicit names", async () => {
  const { env } = makeEnv();

  const unnamedCreate = await worker.fetch(
    post("/devices/pairings", { ttl_seconds: 60 }, "owner-secret"),
    env,
  );
  assert.equal(unnamedCreate.status, 200, "omitting name is the supported unspecified-name contract");
  const unnamedSession = await unnamedCreate.json();
  const unnamedConsume = await worker.fetch(
    post("/devices/pairings/consume", {
      pairing_id: unnamedSession.pairing_id,
      code: unnamedSession.code,
    }),
    env,
  );
  assert.equal(unnamedConsume.status, 200, "consume also accepts an omitted optional name");

  const nullCreate = await worker.fetch(
    post("/devices/pairings", { ttl_seconds: 60, name: null }, "owner-secret"),
    env,
  );
  assert.equal(nullCreate.status, 400);
  assert.equal((await nullCreate.json()).code, "invalid_device_name");

  const namedCreate = await worker.fetch(
    post("/devices/pairings", { ttl_seconds: 60, name: "Nathan Mac" }, "owner-secret"),
    env,
  );
  assert.equal(namedCreate.status, 200);
  const namedSession = await namedCreate.json();

  const nullConsume = await worker.fetch(
    post("/devices/pairings/consume", {
      pairing_id: namedSession.pairing_id,
      code: namedSession.code,
      name: null,
    }),
    env,
  );
  assert.equal(nullConsume.status, 400);
  assert.equal((await nullConsume.json()).code, "invalid_device_name");

  const namedConsume = await worker.fetch(
    post("/devices/pairings/consume", {
      pairing_id: namedSession.pairing_id,
      code: namedSession.code,
      name: "Nathan Mac",
    }),
    env,
  );
  assert.equal(namedConsume.status, 200);
});

test("pairing consume is unauthenticated, single-use, and returns the device secret exactly once", async () => {
  const { env, storage } = makeEnv();
  const create = await worker.fetch(post("/devices/pairings", { ttl_seconds: 60, name: "once-only" }, "owner-secret"), env);
  const session = await create.json();
  const consumeBody = { pairing_id: session.pairing_id, code: session.code };

  const first = await worker.fetch(post("/devices/pairings/consume", consumeBody), env);
  assert.equal(first.status, 200);
  const credential = await first.json();
  assert.match(credential.device_id, /^dev_[0-7][0-9A-HJKMNP-TV-Z]{25}$/);
  assert.equal(credential.workstation_id, credential.device_id, "device_id must be canonical workstation identity");
  assert.match(credential.credential_id, /^cred_[0-9a-f]{32}$/);
  assert.match(credential.device_secret, /^devsec_[0-9a-f]{64}$/);
  assert.equal(JSON.stringify([...storage.map]).includes(credential.device_secret), false);

  const replay = await worker.fetch(post("/devices/pairings/consume", consumeBody), env);
  assert.equal(replay.status, 401);
  assert.equal((await replay.json()).code, "pairing_rejected");
});

test("all enrolled device credentials are equal fleet-admin channels for pairing creation", async () => {
  const { env } = makeEnv();
  const legacyCreate = await worker.fetch(postAsWorkstation(
    "/devices/pairings",
    { ttl_seconds: 60, name: "joined-from-legacy" },
    "prod-real-runtime",
    "legacy-secret",
  ), env);
  assert.equal(legacyCreate.status, 200);

  const consume = await (await worker.fetch(post("/devices/pairings", { ttl_seconds: 60 }, "owner-secret"), env)).json();
  const joined = await worker.fetch(post("/devices/pairings/consume", { pairing_id: consume.pairing_id, code: consume.code }), env);
  const credential = await joined.json();

  const memberCreate = await worker.fetch(postAsWorkstation(
    "/devices/pairings",
    { ttl_seconds: 60 },
    credential.workstation_id,
    credential.device_secret,
  ), env);
  assert.equal(memberCreate.status, 200);
  assert.match((await memberCreate.json()).pairing_id, /^pair_[0-9a-f]{64}$/);
});

test("device inventory is fleet-admin readable and sanitized for every enrolled device", async () => {
  const { env } = makeEnv();
  const joined = await pair(env, "fleet-member");

  const ownerList = await worker.fetch(get("/devices", "owner-secret"), env);
  assert.equal(ownerList.status, 200);
  assert.equal(ownerList.headers.get("cache-control"), "no-store");
  const ownerBody = await ownerList.json();
  assert.equal(ownerBody.ok, true);
  assert.equal(typeof ownerBody.observed_at_ms, "number");
  assert.equal(ownerBody.devices.some((device) => device.device_id === joined.device_id), true);
  assert.equal(JSON.stringify(ownerBody).includes(joined.device_secret), false, "inventory never exposes device credentials");

  const memberList = await worker.fetch(get(
    "/devices",
    joined.device_secret,
    joined.workstation_id,
  ), env);
  assert.equal(memberList.status, 200);
  const memberBody = await memberList.json();
  assert.equal(memberBody.ok, true);
  assert.equal(memberBody.devices.some((device) => device.device_id === joined.device_id), true);
  assert.equal(JSON.stringify(memberBody).includes(joined.device_secret), false);
});

test("device credentials are isolated: credential A never authenticates or routes as device B", async () => {
  const { env, forwarded } = makeEnv();
  const a = await pair(env, "iso-a");
  const b = await pair(env, "iso-b");

  const okA = await worker.fetch(wsRequest(a.workstation_id, a.device_secret), env);
  assert.equal(okA.status, 200, "paired credential authenticates its own workstation");

  const impersonateB = await worker.fetch(wsRequest(b.workstation_id, a.device_secret), env);
  assert.equal(impersonateB.status, 401);
  assert.equal(forwarded.length, 1, "credential A must never reach workstation B");

  const legacyOnNewDevice = await worker.fetch(wsRequest(a.workstation_id, "legacy-secret"), env);
  assert.equal(legacyOnNewDevice.status, 401);
  assert.equal(forwarded.length, 1, "legacy global secret cannot authenticate a paired device");
});

test("a device may revoke only itself with its exact credential", async () => {
  const { env, forwarded } = makeEnv();
  const a = await pair(env, "self-revoke-a");
  const b = await pair(env, "self-revoke-b");

  const impersonatedRevoke = await worker.fetch(new Request("https://edge.example/devices/revoke-self", {
    method: "POST",
    headers: { "content-type": "application/json", authorization: `Bearer ${a.device_secret}` },
    body: JSON.stringify({ workstation_id: b.workstation_id }),
  }), env);
  assert.equal(impersonatedRevoke.status, 401);

  const selfRevoke = await worker.fetch(new Request("https://edge.example/devices/revoke-self", {
    method: "POST",
    headers: { "content-type": "application/json", authorization: `Bearer ${a.device_secret}` },
    body: JSON.stringify({ workstation_id: a.workstation_id }),
  }), env);
  assert.equal(selfRevoke.status, 200);
  assert.equal((await selfRevoke.json()).device_id, a.device_id);

  const reconnect = await worker.fetch(wsRequest(a.workstation_id, a.device_secret), env);
  assert.equal(reconnect.status, 401);
  // The only forward is the teardown kill-switch call to the target WorkstationDO;
  // the revoked credential never reaches it as a reconnect.
  assert.deepEqual(forwarded, ["/internal/revoke"]);
});

test("a joined device may explicitly rename only itself", async () => {
  const { env, registry } = makeEnv();
  const a = await pair(env, "rename-a");
  const b = await pair(env, "rename-b");

  const impersonated = await worker.fetch(postAsWorkstation(
    "/devices/rename-self",
    { workstation_id: b.workstation_id, name: "stolen-name" },
    a.workstation_id,
    a.device_secret,
  ), env);
  assert.equal(impersonated.status, 401);

  const ownerCannotUseSelfRouteForMember = await worker.fetch(postAsWorkstation(
    "/devices/rename-self",
    { workstation_id: b.workstation_id, name: "owner-forced-name" },
    b.workstation_id,
    "owner-secret",
  ), env);
  assert.equal(ownerCannotUseSelfRouteForMember.status, 401);

  const renamed = await worker.fetch(postAsWorkstation(
    "/devices/rename-self",
    { workstation_id: a.workstation_id, name: "qingxian-macbookair" },
    a.workstation_id,
    a.device_secret,
  ), env);
  assert.equal(renamed.status, 200);
  const body = await renamed.json();
  assert.equal(body.device_id, a.device_id);
  assert.equal(body.name, "qingxian-macbookair");
  assert.equal(body.wrote_registry, true);

  const list = await registry.fetch(new Request("https://registry.internal/internal/devices"));
  const devices = (await list.json()).devices;
  assert.equal(devices.find((device) => device.device_id === a.device_id).name, "qingxian-macbookair");
  assert.equal(devices.find((device) => device.device_id === b.device_id).name, "rename-b");
});

test("legacy default device takes its first Link device name and reconnect never overwrites explicit rename", async () => {
  const { env, registry } = makeEnv();
  const first = await worker.fetch(wsRequest("prod-real-runtime", "legacy-secret", "青闲的 MacBook Air"), env);
  assert.equal(first.status, 200);

  let list = await registry.fetch(new Request("https://registry.internal/internal/devices"));
  let legacy = (await list.json()).devices.find((device) => device.workstation_id === "prod-real-runtime");
  assert.equal(legacy.name, "青闲的 MacBook Air");

  const rename = await worker.fetch(postAsWorkstation(
    "/devices/rename-self",
    { workstation_id: "prod-real-runtime", name: "qingxian-macbookair" },
    "prod-real-runtime",
    "legacy-secret",
  ), env);
  assert.equal(rename.status, 200);
  assert.equal((await rename.json()).name, "qingxian-macbookair");

  const reconnect = await worker.fetch(wsRequest("prod-real-runtime", "legacy-secret", "Different Automatic Name"), env);
  assert.equal(reconnect.status, 200);
  list = await registry.fetch(new Request("https://registry.internal/internal/devices"));
  legacy = (await list.json()).devices.find((device) => device.workstation_id === "prod-real-runtime");
  assert.equal(legacy.name, "qingxian-macbookair");
});

test("legacy shared secret is compatibility-only for the default workstation and revoke blocks reconnect", async () => {
  const { env, forwarded } = makeEnv();
  const legacy = await worker.fetch(wsRequest("prod-real-runtime", "legacy-secret"), env);
  assert.equal(legacy.status, 200);
  assert.equal(forwarded.length, 1);

  const paired = await pair(env, "revoked-mac");
  const revoke = await worker.fetch(post("/devices/revoke", { device_id: paired.device_id }, "owner-secret"), env);
  assert.equal(revoke.status, 200);
  assert.equal((await revoke.json()).device_id, paired.device_id);

  const reconnect = await worker.fetch(wsRequest(paired.workstation_id, paired.device_secret), env);
  assert.equal(reconnect.status, 401);
  // forwarded[0] is the legacy connect; forwarded[1] is the revoke teardown call.
  // The revoked credential never reaches the WorkstationDO as a reconnect.
  assert.deepEqual(forwarded, ["/ws/prod-real-runtime", "/internal/revoke"]);
});

test("pairing minted by one Worker deployment fails closed against another", async () => {
  const { registry } = makeEnv();
  const stub = {
    async fetch() { return new Response(JSON.stringify({ ok: true }), { status: 200 }); },
  };
  const envA = makeEnv({ EDGE_PROJECT: "worker-alpha", LINK_SHARED_SECRET: undefined }).env;
  const envB = makeEnv({ EDGE_PROJECT: "worker-beta", LINK_SHARED_SECRET: undefined }).env;
  envA.DEVICE_REGISTRY_DO = namespace(registry);
  envB.DEVICE_REGISTRY_DO = namespace(registry);
  void stub;

  const create = await worker.fetch(post("/devices/pairings", { ttl_seconds: 60 }, "owner-secret"), envA);
  assert.equal(create.status, 200);
  const session = await create.json();

  const crossWorker = await worker.fetch(post(
    "/devices/pairings/consume",
    { pairing_id: session.pairing_id, code: session.code },
  ), envB);
  assert.equal(crossWorker.status, 401);
  assert.equal((await crossWorker.json()).code, "pairing_rejected");

  const sameWorker = await worker.fetch(post(
    "/devices/pairings/consume",
    { pairing_id: session.pairing_id, code: session.code },
  ), envA);
  assert.equal(sameWorker.status, 200);
});

test("approved OAuth Connector is ordinary MCP only and cannot administer the fleet", async () => {
  const oauthStub = {
    async fetch(request) {
      const url = new URL(request.url);
      const body = await request.json();
      if (url.pathname === "/internal/oauth/access/verify") {
        if (body.token === "oauth-chatgpt-token") {
          return new Response(JSON.stringify({ ok: true, client_id: "chatgpt-connector" }), {
            status: 200,
            headers: { "content-type": "application/json" },
          });
        }
        if (body.token === "oauth-legacy-token") {
          return new Response(JSON.stringify({ ok: true, client_id: "legacy-connector" }), {
            status: 200,
            headers: { "content-type": "application/json" },
          });
        }
        if (body.token === "oauth-automation-token") {
          return new Response(JSON.stringify({ ok: true, client_id: "svc_gitlab_pipeline_01" }), {
            status: 200,
            headers: { "content-type": "application/json" },
          });
        }
      }
      if (url.pathname === "/internal/oauth/grant/get" && body.client_id === "chatgpt-connector") {
        return new Response(JSON.stringify({
          ok: true,
          record: {
            client_id: "chatgpt-connector",
            resource: "https://edge.example/mcp",
            scope: "mcp",
            status: "active",
            approved_at_ms: 1,
            approved_by: "device:dev_01M1TEST000000000000000000",
          },
        }), { status: 200, headers: { "content-type": "application/json" } });
      }
      if (url.pathname === "/internal/oauth/grant/get" && body.client_id === "legacy-connector") {
        return new Response(JSON.stringify({ ok: false, code: "not_found" }), {
          status: 404,
          headers: { "content-type": "application/json" },
        });
      }
      if (url.pathname === "/internal/oauth/grant/get" && body.client_id === "svc_gitlab_pipeline_01") {
        return new Response(JSON.stringify({
          ok: true,
          record: {
            client_id: "svc_gitlab_pipeline_01",
            resource: "https://edge.example/mcp",
            scope: "mcp",
            status: "active",
            principal_type: "automation",
            approved_at_ms: 1,
            approved_by: "device:dev_01M1TEST000000000000000000",
          },
        }), { status: 200, headers: { "content-type": "application/json" } });
      }
      return new Response(JSON.stringify({ ok: false }), { status: 401 });
    },
  };

  const { env, forwarded } = makeEnv({
    OAUTH_STORE_DO: namespace(oauthStub),
    DEFAULT_WORKSTATION_ID: "old-dead-macbook", // Old workstation is configured but never contacted
  });

  // 1. Unauthenticated caller cannot create pairing via /mcp or /devices/pairings
  const unauthMcp = await worker.fetch(new Request("https://edge.example/mcp", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      jsonrpc: "2.0",
      id: 1,
      method: "tools/call",
      params: { name: "herdr_call", arguments: { method: "herdr_mcp.device.pair" } },
    }),
  }), env);
  assert.equal(unauthMcp.status, 401);

  const unauthRest = await worker.fetch(post("/devices/pairings", { ttl_seconds: 60 }), env);
  assert.equal(unauthRest.status, 401);
  assert.equal((await unauthRest.json()).code, "pairing_admin_required");

  // 2. A pre-v0.4.6 OAuth token remains valid MCP auth but does not silently
  // gain Worker fleet administration merely because it predates consent.
  const legacyMcp = await worker.fetch(new Request("https://edge.example/mcp", {
    method: "POST",
    headers: {
      "content-type": "application/json",
      authorization: "Bearer oauth-legacy-token",
    },
    body: JSON.stringify({
      jsonrpc: "2.0",
      id: 11,
      method: "tools/call",
      params: { name: "herdr_call", arguments: { method: "herdr_mcp.device.pair" } },
    }),
  }), env);
  assert.equal(legacyMcp.status, 200);
  const legacyMcpBody = await legacyMcp.json();
  assert.equal(legacyMcpBody.result.isError, true);
  assert.equal(legacyMcpBody.result.structuredContent.code, "fleet_admin_required");

  // 3. An approved automation principal can use ordinary MCP but cannot
  // bootstrap/revoke devices or approve Connectors through private methods.
  const automationMcp = await worker.fetch(new Request("https://edge.example/mcp", {
    method: "POST",
    headers: {
      "content-type": "application/json",
      authorization: "Bearer oauth-automation-token",
    },
    body: JSON.stringify({
      jsonrpc: "2.0",
      id: 12,
      method: "tools/call",
      params: { name: "herdr_call", arguments: { method: "herdr_mcp.device.pair" } },
    }),
  }), env);
  assert.equal(automationMcp.status, 200);
  const automationMcpBody = await automationMcp.json();
  assert.equal(automationMcpBody.result.isError, true);
  assert.equal(automationMcpBody.result.structuredContent.code, "fleet_admin_required");

  // 4. An explicitly approved Connector is ordinary MCP only: it cannot
  // create a pairing, revoke a device, or use any REST fleet-admin route.
  const mcpCreate = await worker.fetch(new Request("https://edge.example/mcp", {
    method: "POST",
    headers: {
      "content-type": "application/json",
      authorization: "Bearer oauth-chatgpt-token",
    },
    body: JSON.stringify({
      jsonrpc: "2.0",
      id: 2,
      method: "tools/call",
      params: {
        name: "herdr_call",
        arguments: {
          method: "herdr_mcp.device.pair",
          params: JSON.stringify({ ttl_seconds: 300, name: "new-macbook" }),
        },
      },
    }),
  }), env);
  assert.equal(mcpCreate.status, 200);
  const mcpBody = await mcpCreate.json();
  assert.equal(mcpBody.result.isError, true);
  assert.equal(mcpBody.result.structuredContent.code, "fleet_admin_required");
  assert.equal(forwarded.length, 0, "no workstation DO forward may happen for a denied Connector");

  // 5. The approved Connector cannot revoke a device either.
  const revokeMcp = await worker.fetch(new Request("https://edge.example/mcp", {
    method: "POST",
    headers: {
      "content-type": "application/json",
      authorization: "Bearer oauth-chatgpt-token",
    },
    body: JSON.stringify({
      jsonrpc: "2.0",
      id: 7,
      method: "tools/call",
      params: {
        name: "herdr_call",
        arguments: {
          method: "herdr_mcp.device.revoke",
          params: JSON.stringify({ device_id: "dev_01M1TEST000000000000000000", confirm: true }),
        },
      },
    }),
  }), env);
  assert.equal(revokeMcp.status, 200);
  const revokeBody = await revokeMcp.json();
  assert.equal(revokeBody.result.isError, true);
  assert.equal(revokeBody.result.structuredContent.code, "fleet_admin_required");

  // 6. The approved Connector cannot use the REST fleet-admin route either.
  const restCreate = await worker.fetch(new Request("https://edge.example/devices/pairings", {
    method: "POST",
    headers: {
      "content-type": "application/json",
      authorization: "Bearer oauth-chatgpt-token",
    },
    body: JSON.stringify({ ttl_seconds: 120 }),
  }), env);
  assert.equal(restCreate.status, 401);
  assert.equal((await restCreate.json()).code, "pairing_admin_required");

  // 7. Invalid input regressions on herdr_call herdr_mcp.device.pair are still
  // enforced for a genuine fleet-admin principal (operator dev_bearer).
  const owner = "owner-secret";
  // (a) Top-level device selector is rejected
  const withDevice = await worker.fetch(new Request("https://edge.example/mcp", {
    method: "POST",
    headers: { "content-type": "application/json", authorization: `Bearer ${owner}` },
    body: JSON.stringify({
      jsonrpc: "2.0",
      id: 3,
      method: "tools/call",
      params: { name: "herdr_call", arguments: { method: "herdr_mcp.device.pair", device: "dev_someolddevice" } },
    }),
  }), env);
  const withDeviceBody = await withDevice.json();
  assert.equal(withDeviceBody.result.isError, true);
  assert.equal(withDeviceBody.result.structuredContent.code, "device_selector_not_allowed");

  // (b) Invalid TTL is rejected
  const badTtl = await worker.fetch(new Request("https://edge.example/mcp", {
    method: "POST",
    headers: { "content-type": "application/json", authorization: `Bearer ${owner}` },
    body: JSON.stringify({
      jsonrpc: "2.0",
      id: 4,
      method: "tools/call",
      params: { name: "herdr_call", arguments: { method: "herdr_mcp.device.pair", params: { ttl_seconds: 999 } } },
    }),
  }), env);
  const badTtlBody = await badTtl.json();
  assert.equal(badTtlBody.result.isError, true);
  assert.equal(badTtlBody.result.structuredContent.code, "invalid_pairing_ttl");

  // (c) Invalid name is rejected
  const badName = await worker.fetch(new Request("https://edge.example/mcp", {
    method: "POST",
    headers: { "content-type": "application/json", authorization: `Bearer ${owner}` },
    body: JSON.stringify({
      jsonrpc: "2.0",
      id: 5,
      method: "tools/call",
      params: { name: "herdr_call", arguments: { method: "herdr_mcp.device.pair", params: { name: "" } } },
    }),
  }), env);
  const badNameBody = await badName.json();
  assert.equal(badNameBody.result.isError, true);
  assert.equal(badNameBody.result.structuredContent.code, "invalid_device_name");

  // (d) Unknown parameter key (e.g. ttlSeconds typo) is rejected
  const badKey = await worker.fetch(new Request("https://edge.example/mcp", {
    method: "POST",
    headers: { "content-type": "application/json", authorization: `Bearer ${owner}` },
    body: JSON.stringify({
      jsonrpc: "2.0",
      id: 6,
      method: "tools/call",
      params: { name: "herdr_call", arguments: { method: "herdr_mcp.device.pair", params: { ttlSeconds: 300 } } },
    }),
  }), env);
  const badKeyBody = await badKey.json();
  assert.equal(badKeyBody.result.isError, true);
  assert.equal(badKeyBody.result.structuredContent.code, "invalid_params");
});
