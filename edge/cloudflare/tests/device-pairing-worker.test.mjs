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

function base64UrlUtf8(value) {
  const bytes = new TextEncoder().encode(value);
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/g, "");
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

test("new Connector requires explicit exact owner-device approval; generic owner bearer is insufficient", async () => {
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
  const approvalCode = /<p class="code">(\d{6})<\/p>/.exec(html)?.[1];
  assert.ok(requestId && resumeToken && approvalCode);

  const unauthInspect = await worker.fetch(post("/connectors/inspect", { request_id: requestId }), h.env);
  assert.equal(unauthInspect.status, 401);
  const genericOwner = await worker.fetch(post("/connectors/approve", { request_id: requestId, code: approvalCode }, "owner-secret"), h.env);
  assert.equal(genericOwner.status, 401, "generic MCP/operator bearer must not bootstrap connector approval authority");

  const inspect = await worker.fetch(postAsWorkstation(
    "/connectors/inspect",
    { request_id: requestId },
    "prod-real-runtime",
    "legacy-secret",
  ), h.env);
  assert.equal(inspect.status, 200);
  const details = await inspect.json();
  assert.equal(details.client_id, client.client_id);
  assert.equal(details.client_name, "Test Connector");
  assert.equal(details.redirect_uri, "https://client.example/callback");
  assert.equal(details.scope, "mcp");

  const approved = await worker.fetch(postAsWorkstation(
    "/connectors/approve",
    { request_id: requestId, code: approvalCode },
    "prod-real-runtime",
    "legacy-secret",
  ), h.env);
  assert.equal(approved.status, 200);
  assert.equal((await approved.json()).client_id, client.client_id);

  const target = await pair(h.env, "webchat-grant-target");
  const grantInput = {
    client_id: client.client_id,
    device_id: target.device_id,
    endpoint_ref: `be_${"a".repeat(64)}`,
    provider: "chatgpt",
    account_ref: `br_${"b".repeat(64)}`,
    allowed: true,
  };
  const genericGrant = await worker.fetch(post("/connectors/webchat-control", grantInput, "owner-secret"), h.env);
  assert.equal(genericGrant.status, 401, "generic MCP/operator bearer cannot widen WebChat Control");
  const ownerGrant = await worker.fetch(postAsWorkstation(
    "/connectors/webchat-control",
    grantInput,
    "prod-real-runtime",
    "legacy-secret",
  ), h.env);
  assert.equal(ownerGrant.status, 200);
  const ownerGrantBody = await ownerGrant.json();
  assert.equal(ownerGrantBody.action, "connector_webchat_control_set");
  assert.deepEqual(ownerGrantBody.grants, [{
    device_id: target.device_id,
    endpoint_ref: grantInput.endpoint_ref,
    provider: "chatgpt",
    account_ref: grantInput.account_ref,
  }]);

  const storedGrant = await oauth.fetch(new Request("https://oauth.internal/internal/oauth/grant/get", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ client_id: client.client_id }),
  }));
  assert.equal(storedGrant.status, 200);
  assert.deepEqual((await storedGrant.json()).record.webchat_control, ownerGrantBody.grants);

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

  const genericRevoke = await worker.fetch(
    post("/connectors/revoke", { client_id: client.client_id }, "owner-secret"),
    h.env,
  );
  assert.equal(genericRevoke.status, 401, "generic MCP/operator bearer must not revoke connector grants");

  const revoked = await worker.fetch(postAsWorkstation(
    "/connectors/revoke",
    { client_id: client.client_id },
    "prod-real-runtime",
    "legacy-secret",
  ), h.env);
  assert.equal(revoked.status, 200);
  const revokedBody = await revoked.json();
  assert.equal(revokedBody.ok, true);
  assert.equal(revokedBody.action, "connector_revoke");
  assert.equal(revokedBody.client_id, client.client_id);

  const storedRevokedGrant = await oauth.fetch(new Request("https://oauth.internal/internal/oauth/grant/get", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ client_id: client.client_id }),
  }));
  assert.equal(storedRevokedGrant.status, 200);
  const storedRevokedGrantBody = await storedRevokedGrant.json();
  assert.equal(storedRevokedGrantBody.record.status, "revoked");
});

test("pairing creation requires owner auth and returns one-time material with worker origin metadata", async () => {
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

test("joined device credentials cannot create pairings; only the owner workstation link may", async () => {
  const { env } = makeEnv();
  const ownerCreate = await worker.fetch(postAsWorkstation(
    "/devices/pairings",
    { ttl_seconds: 60, name: "joined-from-owner" },
    "prod-real-runtime",
    "legacy-secret",
  ), env);
  assert.equal(ownerCreate.status, 200);

  const consume = await (await worker.fetch(post("/devices/pairings", { ttl_seconds: 60 }, "owner-secret"), env)).json();
  const joined = await worker.fetch(post("/devices/pairings/consume", { pairing_id: consume.pairing_id, code: consume.code }), env);
  const credential = await joined.json();

  const memberCreate = await worker.fetch(postAsWorkstation(
    "/devices/pairings",
    { ttl_seconds: 60 },
    credential.workstation_id,
    credential.device_secret,
  ), env);
  assert.equal(memberCreate.status, 401);
  assert.equal((await memberCreate.json()).code, "pairing_admin_required");
});

test("device inventory is owner-readable, sanitized, and denied to joined member credentials", async () => {
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
  assert.equal(memberList.status, 401);
  assert.equal((await memberList.json()).code, "device_inventory_admin_required");
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

test("OAuth owner can create pairing via herdr_call herdr_mcp.device.pair with no old workstation, consume unchanged, unauthenticated rejected", async () => {
  const oauthStub = {
    async fetch(request) {
      const url = new URL(request.url);
      if (url.pathname === "/internal/oauth/access/verify") {
        const body = await request.json();
        if (body.token === "oauth-chatgpt-token") {
          return new Response(JSON.stringify({ ok: true, client_id: "chatgpt-connector" }), {
            status: 200,
            headers: { "content-type": "application/json" },
          });
        }
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

  // 2. OAuth owner creates pairing via herdr_call without any old workstation credential or header
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
  assert.equal(mcpBody.result.isError, undefined);
  const pairResult = mcpBody.result.structuredContent;
  assert.equal(pairResult.ok, true);
  assert.match(pairResult.pairing_id, /^pair_[0-9a-f]{64}$/);
  assert.match(pairResult.code, /^[0-9]{6}$/);
  assert.equal(typeof pairResult.expires_at_ms, "number");
  assert.equal(pairResult.pairing_address, `https://edge.example/pair#${pairResult.pairing_id}`);
  assert.equal(pairResult.pairing_address.includes(pairResult.code), false, "code must never appear in pairing address URL/fragment");
  assert.ok(pairResult.instructions.includes("herdr-mcp worker connect"));
  assert.equal(forwarded.length, 0, "no workstation DO forward may happen during pair creation");

  // 3. New computer consumes pairing via /devices/pairings/consume (unchanged)
  const consumeResp = await worker.fetch(post("/devices/pairings/consume", {
    pairing_id: pairResult.pairing_id,
    code: pairResult.code,
  }), env);
  assert.equal(consumeResp.status, 200);
  const creds = await consumeResp.json();
  assert.equal(creds.ok, true);
  assert.match(creds.device_id, /^dev_[0-7][0-9A-HJKMNP-TV-Z]{25}$/);
  assert.match(creds.credential_id, /^cred_[0-9a-f]{32}$/);
  assert.match(creds.device_secret, /^devsec_[0-9a-f]{64}$/);

  // Single-use replay fails closed
  const replay = await worker.fetch(post("/devices/pairings/consume", {
    pairing_id: pairResult.pairing_id,
    code: pairResult.code,
  }), env);
  assert.equal(replay.status, 401);
  assert.equal((await replay.json()).code, "pairing_rejected");

  // 4. The same OAuth owner conversation can permanently revoke the newly
  // enrolled immutable device at Edge, without forwarding to any workstation.
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
          params: JSON.stringify({ device_id: creds.device_id, confirm: true }),
        },
      },
    }),
  }), env);
  assert.equal(revokeMcp.status, 200);
  const revokeBody = await revokeMcp.json();
  assert.equal(revokeBody.result.isError, undefined);
  assert.equal(revokeBody.result.structuredContent.ok, true);
  assert.equal(revokeBody.result.structuredContent.revoked, true);
  assert.equal(revokeBody.result.structuredContent.device_id, creds.device_id);
  assert.equal(typeof revokeBody.result.structuredContent.revoked_at_ms, "number");
  assert.deepEqual(forwarded, ["/internal/revoke"], "Edge-local revoke may only use the dedicated WorkstationDO revoke fence, never /internal/forward");

  // 5. OAuth owner can also create pairing via POST /devices/pairings without workstation headers
  const restCreate = await worker.fetch(new Request("https://edge.example/devices/pairings", {
    method: "POST",
    headers: {
      "content-type": "application/json",
      authorization: "Bearer oauth-chatgpt-token",
    },
    body: JSON.stringify({ ttl_seconds: 120 }),
  }), env);
  assert.equal(restCreate.status, 200);
  const restBody = await restCreate.json();
  assert.equal(restBody.ok, true);
  assert.match(restBody.pairing_id, /^pair_[0-9a-f]{64}$/);

  // 6. Invalid input regressions on herdr_call herdr_mcp.device.pair:
  // (a) Top-level device selector is rejected
  const withDevice = await worker.fetch(new Request("https://edge.example/mcp", {
    method: "POST",
    headers: { "content-type": "application/json", authorization: "Bearer oauth-chatgpt-token" },
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
    headers: { "content-type": "application/json", authorization: "Bearer oauth-chatgpt-token" },
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
    headers: { "content-type": "application/json", authorization: "Bearer oauth-chatgpt-token" },
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
    headers: { "content-type": "application/json", authorization: "Bearer oauth-chatgpt-token" },
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
