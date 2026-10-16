import { test } from "node:test";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";

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
  async getAlarm() { return this.alarmSetAt ?? null; }
  async setAlarm(at) { this.alarmSetAt = at; } // #630 migrate confirm first-schedules the idempotency sweep
  async deleteAlarm() { this.alarmSetAt = undefined; }
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

test("user keeps Worker semantic credentials at Edge | Given an enrolled device and Worker-wide route pool | When semantic evaluation is requested | Then authentication is required and credentials never return", async () => {
  const routes = [{
    name: "edge_fast",
    protocol: "decision",
    url: "https://api.typesafe.ai/v1/systemone",
    model: "jev-edge",
    api_key: "edge-typesafe-secret",
  }];
  const h = makeEnv({
    HERDR_SEMANTIC_ROUTES: JSON.stringify(routes),
  });
  const paired = await pair(h.env, "semantic-edge");
  const body = {
    state: "The task edits Rust code.",
    questions: {
      edit: {
        type: "noul",
        instructions: "Does this task require source edits?",
        criteria: { true: "Source edits required", false: "No source edits required" },
      },
    },
  };

  const denied = await worker.fetch(
    postAsWorkstation("/semantic/systemone", body, paired.workstation_id, "wrong-device-secret"),
    h.env,
  );
  assert.equal(denied.status, 401);

  const status = await worker.fetch(
    get("/semantic/status", paired.device_secret, paired.workstation_id),
    h.env,
  );
  assert.equal(status.status, 200);
  assert.deepEqual(await status.json(), {
    ok: true,
    available: true,
    evaluate_available: true,
    chat_available: false,
    routes: [{
      name: "edge_fast",
      protocol: "decision",
      model: "jev-edge",
    }],
  });

  const unavailableStatus = await worker.fetch(
    get("/semantic/status", paired.device_secret, paired.workstation_id),
    { ...h.env, HERDR_SEMANTIC_ROUTES: undefined },
  );
  assert.equal(unavailableStatus.status, 200);
  assert.deepEqual(await unavailableStatus.json(), {
    ok: true,
    available: false,
    evaluate_available: false,
    chat_available: false,
    routes: [],
  });

  const originalFetch = globalThis.fetch;
  let upstreamAuthorization = "";
  let upstreamBody = null;
  globalThis.fetch = async (input, init = {}) => {
    const url = input instanceof Request ? input.url : String(input);
    assert.equal(url, "https://api.typesafe.ai/v1/systemone");
    upstreamAuthorization = new Headers(init.headers).get("authorization") ?? "";
    upstreamBody = JSON.parse(String(init.body));
    return new Response(JSON.stringify({
      model: "jev-edge",
      answers: { edit: { type: "noul", noul: 0.94 } },
    }), {
      status: 200,
      headers: { "content-type": "application/json" },
    });
  };

  try {
    const response = await worker.fetch(
      postAsWorkstation("/semantic/systemone", body, paired.workstation_id, paired.device_secret),
      h.env,
    );
    assert.equal(response.status, 200);
    const payload = await response.json();
    assert.equal(payload.answers.edit.noul, 0.94);
    assert.equal(upstreamAuthorization, "Bearer edge-typesafe-secret");
    assert.equal(upstreamBody.model, "jev-edge");
    assert.equal(JSON.stringify(payload).includes("edge-typesafe-secret"), false);
  } finally {
    globalThis.fetch = originalFetch;
  }
});

test("user keeps semantic failover bounded | Given two independent route credentials and a rate-limited first route | When semantic evaluation runs | Then the next route succeeds without leaking either secret", async () => {
  const routes = [
    {
      name: "direct-a",
      protocol: "decision",
      url: "https://typesafe.example/systemone",
      model: "jev-a",
      api_key: "route-secret-a",
    },
    {
      name: "openrouter-b",
      protocol: "decision",
      url: "https://openrouter.example/api/alpha/decisions",
      model: "~typesafe/jev-latest",
      api_key: "route-secret-b",
    },
  ];
  const h = makeEnv({
    HERDR_SEMANTIC_ROUTES: JSON.stringify(routes),
  });
  const paired = await pair(h.env, "semantic-routes");
  const body = {
    state: "Choose a useful route.",
    questions: {
      useful: {
        type: "noul",
        instructions: "Is this useful?",
        criteria: { true: "Useful", false: "Not useful" },
      },
    },
  };

  const status = await worker.fetch(
    get("/semantic/status", paired.device_secret, paired.workstation_id),
    h.env,
  );
  assert.equal(status.status, 200);
  const statusPayload = await status.json();
  assert.equal(statusPayload.available, true);
  assert.deepEqual(
    statusPayload.routes.map(({ name, protocol, model }) => ({ name, protocol, model })),
    routes.map(({ name, protocol, model }) => ({ name, protocol, model })),
  );
  assert.equal(JSON.stringify(statusPayload).includes("route-secret"), false);

  const originalFetch = globalThis.fetch;
  const calls = [];
  globalThis.fetch = async (input, init = {}) => {
    const url = input instanceof Request ? input.url : String(input);
    const authorization = new Headers(init.headers).get("authorization") ?? "";
    const requestBody = JSON.parse(String(init.body));
    calls.push({ url, authorization, requestBody });
    if (calls.length === 1) {
      return new Response(JSON.stringify({ error: "rate limited" }), { status: 429 });
    }
    return new Response(JSON.stringify({
      model: "jev-fallback",
      answers: { useful: { type: "noul", noul: 0.92 } },
    }), {
      status: 200,
      headers: { "content-type": "application/json" },
    });
  };

  try {
    const response = await worker.fetch(
      postAsWorkstation("/semantic/systemone", body, paired.workstation_id, paired.device_secret),
      h.env,
    );
    assert.equal(response.status, 200);
    const payload = await response.json();
    assert.equal(payload.answers.useful.noul, 0.92);
    assert.equal(calls.length, 2);
    assert.notEqual(calls[0].authorization, calls[1].authorization);
    assert.deepEqual(
      new Set(calls.map((call) => call.authorization)),
      new Set(["Bearer route-secret-a", "Bearer route-secret-b"]),
    );
    assert.equal(JSON.stringify(payload).includes("route-secret"), false);
  } finally {
    globalThis.fetch = originalFetch;
  }
});

test("user uses one semantic route schema for chat | Given a Worker-wide openai-chat route | When semantic chat is requested | Then the configured URL model credential and content are used", async () => {
  const h = makeEnv({
    HERDR_SEMANTIC_ROUTES: JSON.stringify([{
      name: "chat_primary",
      protocol: "openai-chat",
      url: "https://chat.example/v1/chat/completions",
      model: "chat-model",
      api_key: "chat-secret",
    }]),
  });
  const paired = await pair(h.env, "semantic-chat");

  const status = await worker.fetch(
    get("/semantic/status", paired.device_secret, paired.workstation_id),
    h.env,
  );
  assert.equal(status.status, 200);
  assert.deepEqual(await status.json(), {
    ok: true,
    available: true,
    evaluate_available: false,
    chat_available: true,
    routes: [{
      name: "chat_primary",
      protocol: "openai-chat",
      model: "chat-model",
    }],
  });

  const originalFetch = globalThis.fetch;
  let upstreamUrl = "";
  let upstreamAuthorization = "";
  let upstreamBody;
  globalThis.fetch = async (input, init = {}) => {
    upstreamUrl = input instanceof Request ? input.url : String(input);
    upstreamAuthorization = new Headers(init.headers).get("authorization") ?? "";
    upstreamBody = JSON.parse(String(init.body));
    return new Response(JSON.stringify({
      choices: [{ message: { content: "continue" } }],
      usage: { total_tokens: 7 },
    }), {
      status: 200,
      headers: { "content-type": "application/json" },
    });
  };

  try {
    const response = await worker.fetch(
      postAsWorkstation(
        "/semantic/chat",
        { messages: [{ role: "user", content: "continue?" }] },
        paired.workstation_id,
        paired.device_secret,
      ),
      h.env,
    );
    assert.equal(response.status, 200);
    assert.equal(upstreamUrl, "https://chat.example/v1/chat/completions");
    assert.equal(upstreamAuthorization, "Bearer chat-secret");
    assert.equal(upstreamBody.model, "chat-model");
    assert.deepEqual(await response.json(), {
      model: "chat-model",
      content: "continue",
      usage: { total_tokens: 7 },
    });
  } finally {
    globalThis.fetch = originalFetch;
  }
});

test("user receives normalized Vercel evaluation | Given a decision-vercel Jev route | When a Noul question is evaluated | Then boolean wire output is normalized to Noul", async () => {
  const h = makeEnv({
    HERDR_SEMANTIC_ROUTES: JSON.stringify([{
      name: "fast_gateway",
      protocol: "decision-vercel",
      url: "https://ai-gateway.vercel.sh/v4/ai/evaluation-model",
      model: "typesafe-ai/jev",
      api_key: "vercel-secret",
    }]),
  });
  const paired = await pair(h.env, "semantic-vercel");
  const body = {
    state: "The task is complete.",
    questions: {
      done: {
        type: "noul",
        instructions: "Is the task complete?",
        criteria: { true: "Complete", false: "Incomplete" },
      },
    },
  };

  const originalFetch = globalThis.fetch;
  let upstreamHeaders;
  let upstreamBody;
  globalThis.fetch = async (_input, init = {}) => {
    upstreamHeaders = new Headers(init.headers);
    upstreamBody = JSON.parse(String(init.body));
    return new Response(JSON.stringify({
      answers: { done: { type: "boolean", probability: 0.91 } },
    }), {
      status: 200,
      headers: { "content-type": "application/json" },
    });
  };

  try {
    const response = await worker.fetch(
      postAsWorkstation("/semantic/systemone", body, paired.workstation_id, paired.device_secret),
      h.env,
    );
    assert.equal(response.status, 200);
    const payload = await response.json();
    assert.equal(upstreamHeaders.get("authorization"), "Bearer vercel-secret");
    assert.equal(upstreamHeaders.get("ai-evaluation-model-specification-version"), "4");
    assert.equal(upstreamHeaders.get("ai-model-id"), "typesafe-ai/jev");
    assert.equal(upstreamBody.questions.done.type, "boolean");
    assert.deepEqual(payload, {
      model: "typesafe-ai/jev",
      answers: { done: { type: "noul", noul: 0.91 } },
    });
  } finally {
    globalThis.fetch = originalFetch;
  }
});

test("health and info expose the same explicit first-party public contract identity", async () => {
  for (const [edgeEnv, expectedEpoch] of [
    ["dev", 8],
    ["prod", 8],
    [undefined, 3],
    ["unknown", 3],
  ]) {
    const h = makeEnv(edgeEnv === undefined ? {} : { EDGE_ENV: edgeEnv });
    const health = await (await worker.fetch(get("/health"), h.env)).json();
    const info = await (await worker.fetch(get("/info"), h.env)).json();
    assert.equal(health.contractEpoch, expectedEpoch);
    assert.equal(info.publicContract.epoch, expectedEpoch);
    assert.equal(health.contractHash, info.publicContract.hash);
    // Rolling-rollback fence: /health must stay parseable by a Link built
    // before the current epoch, so it publishes the frozen epoch-2 catalog view
    // first and the rollback-compatible runtime identity (epoch 3) in the field
    // that an older Link reads first. Whether this Edge accepts the current
    // epoch is proven only by the authenticated hello_ack; /info reports the
    // current + previous identities for humans and probes.
    assert.equal(health.runtimeContractEpoch, 2);
    assert.equal(
      health.runtimeContractHash,
      "sha256:7da23ad2ec8e7703d6380062126ba797218bde9e7711138c6b3e0ca6592efbf8",
    );
    assert.equal(health.currentRuntimeContractEpoch, 4);
    assert.equal(
      health.currentRuntimeContractHash,
      "sha256:1f4d272cedb3334b3e17e08080793f6ed81a03dccffba2f6434f149b10e2e135",
    );
    assert.equal(info.runtimeContract.epoch, 5);
    assert.equal(info.previousRuntimeContract.epoch, 4);
    assert.equal(
      info.previousRuntimeContract.hash,
      "sha256:1f4d272cedb3334b3e17e08080793f6ed81a03dccffba2f6434f149b10e2e135",
    );
  }
});

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

  const approvedInventory = await worker.fetch(new Request("https://edge.example/connectors", {
    headers: { authorization: "Bearer owner-secret" },
  }), h.env);
  assert.equal(approvedInventory.status, 200);
  const connectorId = (await approvedInventory.json()).connectors[0].connector_id;
  assert.match(connectorId, /^conn_[A-Za-z0-9_-]+$/);

  const target = await pair(h.env, "webchat-grant-target");
  const grantInput = {
    connector_id: connectorId,
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
    connector_id: connectorId,
    grant_generation: 1,
    device_id: target.device_id,
    endpoint_ref: grantInput.endpoint_ref,
    provider: "chatgpt",
    account_ref: grantInput.account_ref,
  }]);

  const deviceScopeInput = {
    connector_id: connectorId, device_id: target.device_id,
    endpoint_ref: "*", provider: "*", account_ref: "*", allowed: true,
  };
  const operatorScope = await worker.fetch(post(
    "/connectors/webchat-control", deviceScopeInput, "owner-secret",
  ), h.env);
  assert.equal(operatorScope.status, 401, "ordinary owner bearer cannot issue device-wide authority");
  const legacyScope = await worker.fetch(postAsWorkstation(
    "/connectors/webchat-control", deviceScopeInput, "prod-real-runtime", "legacy-secret",
  ), h.env);
  assert.equal(legacyScope.status, 403, "legacy owner without enrolled target identity cannot widen it");
  for (const scopeInput of [
    { ...deviceScopeInput, account_ref: undefined },
    { ...deviceScopeInput, endpoint_ref: " * " },
    { ...deviceScopeInput, provider: " * " },
    { ...deviceScopeInput, account_ref: " * " },
  ]) {
    const normalizedLegacyScope = await worker.fetch(postAsWorkstation(
      "/connectors/webchat-control", scopeInput, "prod-real-runtime", "legacy-secret",
    ), h.env);
    assert.equal(normalizedLegacyScope.status, 403,
      "normalized device scope still requires an enrolled device credential");
  }
  const exactOwnerEnv = { ...h.env, DEFAULT_WORKSTATION_ID: target.device_id };
  const malformedScope = await worker.fetch(postAsWorkstation(
    "/connectors/webchat-control", { ...deviceScopeInput, provider: "grok" },
    target.device_id, target.device_secret,
  ), exactOwnerEnv);
  assert.equal(malformedScope.status, 400);
  const otherTarget = await pair(h.env, "another-webchat-grant-device");
  const crossFleetScope = await worker.fetch(postAsWorkstation(
    "/connectors/webchat-control", { ...deviceScopeInput, device_id: otherTarget.device_id },
    target.device_id, target.device_secret,
  ), exactOwnerEnv);
  assert.equal(crossFleetScope.status, 200, "credential holder may device-wide grant another ACTIVE same-fleet device");
  const revokedScope = await worker.fetch(postAsWorkstation(
    "/connectors/webchat-control", { ...deviceScopeInput, device_id: "dev_01ARZ3NDEKTSV4RRFFQ69G5FAV" },
    target.device_id, target.device_secret,
  ), exactOwnerEnv);
  assert.equal(revokedScope.status, 403, "unknown or non-active fleet target is rejected");
  const revokeOther = await worker.fetch(postAsWorkstation(
    "/connectors/webchat-control", { ...deviceScopeInput, device_id: otherTarget.device_id, allowed: false },
    target.device_id, target.device_secret,
  ), exactOwnerEnv);
  assert.equal(revokeOther.status, 200, "same-fleet cleanup revokes the cross-device grant again");
  const wide = await worker.fetch(postAsWorkstation(
    "/connectors/webchat-control", deviceScopeInput, target.device_id, target.device_secret,
  ), exactOwnerEnv);
  assert.equal(wide.status, 200, "the exact enrolled owner can issue a device-wide grant");
  const normalizedOwnerScope = await worker.fetch(postAsWorkstation(
    "/connectors/webchat-control",
    { ...deviceScopeInput, endpoint_ref: " * ", account_ref: undefined },
    target.device_id, target.device_secret,
  ), exactOwnerEnv);
  assert.equal(normalizedOwnerScope.status, 200, "enrolled owner may grant normalized device scope");
  assert.equal((await normalizedOwnerScope.json()).scope, "device");
  const wideGrants = (await wide.json()).grants;
  assert.equal(wideGrants.filter(g => g.endpoint_ref === "*").length, 1);
  assert.equal(wideGrants.length, 2, "existing narrow grant is preserved");
  const deniedWide = await worker.fetch(postAsWorkstation(
    "/connectors/webchat-control", { ...deviceScopeInput, allowed: false },
    target.device_id, target.device_secret,
  ), exactOwnerEnv);
  assert.equal(deniedWide.status, 200);
  assert.deepEqual((await deniedWide.json()).grants, ownerGrantBody.grants);

  const storedGrant = await oauth.fetch(new Request("https://oauth.internal/internal/oauth/grant/get", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ client_id: client.client_id }),
  }));
  assert.equal(storedGrant.status, 200);
  assert.deepEqual((await storedGrant.json()).record.webchat_control, ownerGrantBody.grants);

  const pageAssistInput = {
    connector_id: connectorId,
    device_id: target.device_id,
    endpoint_ref: `be_${"c".repeat(64)}`,
    allowed: true,
  };
  const genericPageAssist = await worker.fetch(post("/connectors/page-assist", pageAssistInput, "owner-secret"), h.env);
  assert.equal(genericPageAssist.status, 401, "generic MCP/operator bearer cannot widen Page Assist");
  const ownerPageAssist = await worker.fetch(postAsWorkstation(
    "/connectors/page-assist",
    pageAssistInput,
    "prod-real-runtime",
    "legacy-secret",
  ), h.env);
  assert.equal(ownerPageAssist.status, 200);
  const ownerPageAssistBody = await ownerPageAssist.json();
  assert.equal(ownerPageAssistBody.action, "connector_page_assist_set");
  assert.deepEqual(ownerPageAssistBody.grants, [{
    connector_id: connectorId,
    grant_generation: 1,
    device_id: target.device_id,
    endpoint_ref: pageAssistInput.endpoint_ref,
  }]);

  const storedPageAssist = await oauth.fetch(new Request("https://oauth.internal/internal/oauth/grant/get", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ client_id: client.client_id }),
  }));
  assert.equal(storedPageAssist.status, 200);
  assert.deepEqual((await storedPageAssist.json()).record.page_assist, ownerPageAssistBody.grants);

  const ownerPageAssistDeny = await worker.fetch(postAsWorkstation(
    "/connectors/page-assist",
    { ...pageAssistInput, allowed: false },
    "prod-real-runtime",
    "legacy-secret",
  ), h.env);
  assert.equal(ownerPageAssistDeny.status, 200);
  assert.deepEqual((await ownerPageAssistDeny.json()).grants, [], "owner revoke removes the exact Page Assist tuple immediately");

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

  const cancelRegistration = await worker.fetch(new Request("https://edge.example/oauth/register", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      redirect_uris: ["https://client.example/callback"],
      token_endpoint_auth_method: "none",
      client_name: "Cancel Connector",
    }),
  }), h.env);
  assert.equal(cancelRegistration.status, 201);
  const cancelClient = await cancelRegistration.json();

  const cancelAuthorize = new URL(authorize);
  cancelAuthorize.searchParams.set("client_id", cancelClient.client_id);
  cancelAuthorize.searchParams.set("code_challenge", "B".repeat(43));
  cancelAuthorize.searchParams.set("state", "state-cancel");
  const cancelPage = await worker.fetch(new Request(cancelAuthorize), h.env);
  assert.equal(cancelPage.status, 200);
  const cancelHtml = await cancelPage.text();
  const cancelRequestId = /const requestId="([A-Za-z0-9_-]+)";/.exec(cancelHtml)?.[1];
  const cancelResumeToken = /const resumeToken="([A-Za-z0-9_-]+)";/.exec(cancelHtml)?.[1];
  assert.ok(cancelRequestId && cancelResumeToken);
  const unauthCancel = await worker.fetch(post("/connectors/cancel", { request_id: cancelRequestId }), h.env);
  assert.equal(unauthCancel.status, 401);
  const cancelled = await worker.fetch(post("/connectors/cancel", { request_id: cancelRequestId }, "owner-secret"), h.env);
  assert.equal(cancelled.status, 200);
  assert.equal((await cancelled.json()).request_id, cancelRequestId);
  const cancelledPoll = new URL("https://edge.example/oauth/authorize/poll");
  cancelledPoll.searchParams.set("request_id", cancelRequestId);
  cancelledPoll.searchParams.set("resume_token", cancelResumeToken);
  assert.equal((await worker.fetch(new Request(cancelledPoll), h.env)).status, 410);
  // Keep the legacy-inventory assertions below scoped to their original
  // fixtures; this disposable DCR registration exists only to exercise cancel.
  await oauthStorage.delete(`client:${cancelClient.client_id}`);

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
  await oauthStorage.put("refresh:unattributed-live", {
    client_id: "https://missing.example/oauth/client-metadata.json",
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
  assert.equal(inventoryBody.connectors[0].connector_id, connectorId);
  assert.equal(inventoryBody.legacy_clients.length, 2);
  const legacy = inventoryBody.legacy_clients.find((entry) => entry.client_id === "https://legacy.example/oauth/client-metadata.json");
  assert.deepEqual(legacy, {
    client_id: "https://legacy.example/oauth/client-metadata.json",
    client_name: "Legacy WebChat",
    issued_at: nowSec - 60,
    created_at_ms: (nowSec - 60) * 1000,
    last_used_at_ms: null,
    grant_origin: "pre_v0_4_6_legacy",
    grant_status: null,
    registration_state: "active_credentials",
    active_access_tokens: 1,
    active_refresh_tokens: 1,
  });
  const unattributed = inventoryBody.legacy_clients.find((entry) => entry.client_id === "https://missing.example/oauth/client-metadata.json");
  assert.deepEqual(unattributed, {
    client_id: "https://missing.example/oauth/client-metadata.json",
    client_name: null,
    issued_at: null,
    created_at_ms: null,
    last_used_at_ms: null,
    grant_origin: "unattributed_existing_credentials",
    grant_status: null,
    registration_state: "active_credentials",
    active_access_tokens: 0,
    active_refresh_tokens: 1,
  });
  assert.equal(legacy.client_secret_hash, undefined);
  assert.equal(inventoryBody.token_counts.active_access, 1);
  assert.equal(inventoryBody.token_counts.active_refresh, 2);
  assert.equal(
    inventoryBody.legacy_clients.reduce((sum, entry) => sum + entry.active_refresh_tokens, 0),
    inventoryBody.token_counts.active_refresh,
  );

  const genericRevoke = await worker.fetch(
    post("/connectors/revoke", { client_id: client.client_id }, "unauthorized-bearer"),
    h.env,
  );
  assert.equal(genericRevoke.status, 401, "generic MCP/operator bearer must not revoke connector grants");

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

test("browser authority is fenced to one exact Connector instance even when client_id is shared", async () => {
  const h = makeEnv();
  const target = await pair(h.env, "exact-connector-browser-authority");
  const connectorA = "conn_shared_client_a1";
  const connectorB = "conn_shared_client_b1";
  const endpointA = `be_${"a".repeat(64)}`;
  const endpointB = `be_${"b".repeat(64)}`;
  const forwarded = [];
  const workstationStub = {
    async fetch(request) {
      forwarded.push(JSON.parse(await request.text()));
      return new Response(JSON.stringify({
        status: "ok",
        completion: { status: "ok", result: { ok: true } },
      }), { status: 200, headers: { "content-type": "application/json" } });
    },
  };
  const oauthPaths = [];
  const oauthStub = {
    async fetch(request) {
      const url = new URL(request.url);
      oauthPaths.push(url.pathname);
      const body = await request.json();
      if (url.pathname === "/internal/oauth/access/verify") {
        if (body.token === "connector-a-token") {
          return new Response(JSON.stringify({
            ok: true,
            client_id: "shared-chatgpt-client",
            connector_id: connectorA,
            grant_generation: 1,
            principal_type: "connector",
            page_assist: [{ device_id: target.device_id, endpoint_ref: endpointA }],
          }), { status: 200, headers: { "content-type": "application/json" } });
        }
        if (body.token === "connector-b-token") {
          return new Response(JSON.stringify({
            ok: true,
            client_id: "shared-chatgpt-client",
            connector_id: connectorB,
            grant_generation: 1,
            principal_type: "connector",
            page_assist: [{ device_id: target.device_id, endpoint_ref: endpointB }],
          }), { status: 200, headers: { "content-type": "application/json" } });
        }
        if (body.token === "legacy-shared-client-token") {
          return new Response(JSON.stringify({
            ok: true,
            client_id: "shared-chatgpt-client",
          }), { status: 200, headers: { "content-type": "application/json" } });
        }
      }
      if (url.pathname === "/internal/oauth/grant/get" && body.client_id === "shared-chatgpt-client") {
        return new Response(JSON.stringify({
          ok: true,
          record: {
            client_id: "shared-chatgpt-client",
            status: "active",
            resource: "https://edge.example/mcp",
            scope: "mcp",
            approved_at_ms: 1,
            approved_by: "device:owner",
            page_assist: [
              { connector_id: connectorA, grant_generation: 1, device_id: target.device_id, endpoint_ref: endpointA },
              { connector_id: connectorB, grant_generation: 1, device_id: target.device_id, endpoint_ref: endpointB },
            ],
          },
        }), { status: 200, headers: { "content-type": "application/json" } });
      }
      return new Response(JSON.stringify({ ok: false, code: "not_found" }), {
        status: 404,
        headers: { "content-type": "application/json" },
      });
    },
  };
  h.env.OAUTH_STORE_DO = namespace(oauthStub);
  h.env.WORKSTATION_DO = namespace(workstationStub);

  const call = (token, id) => worker.fetch(new Request("https://edge.example/mcp", {
    method: "POST",
    headers: { "content-type": "application/json", authorization: `Bearer ${token}` },
    body: JSON.stringify({
      jsonrpc: "2.0",
      id,
      method: "tools/call",
      params: {
        name: "herdr_call",
        arguments: {
          method: "herdr_mcp.page_assist",
          device: target.device_id,
          params: JSON.stringify({
            endpoint_ref: endpointA,
            action: "inspect",
            target_origin: "https://example.com",
          }),
        },
      },
    }),
  }), h.env);

  assert.equal((await call("connector-a-token", 71)).status, 200);
  assert.deepEqual(forwarded[0].trace?.page_assist_grants, [{
    endpoint_ref: endpointA,
  }], "Connector A receives only its own exact browser authority after device routing strips device metadata");

  assert.equal((await call("connector-b-token", 72)).status, 200);
  assert.deepEqual(forwarded[1].trace?.page_assist_grants, [{
    endpoint_ref: endpointB,
  }], "Connector B cannot inherit Connector A's Page Assist endpoint even with the same public client_id");

  assert.equal((await call("legacy-shared-client-token", 73)).status, 200);
  assert.equal(forwarded[2].trace?.page_assist_grants, undefined,
    "client-only legacy identity cannot inherit browser authority from any Connector instance");
  assert.deepEqual(oauthPaths, [
    "/internal/oauth/access/verify",
    "/internal/oauth/access/verify",
    "/internal/oauth/access/verify",
  ], "each MCP call must consume exactly one OAuthStoreDO request; no post-verify grant/get round trips");
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
  assert.equal(credential.recovered_existing, false);
  assert.match(credential.device_id, /^dev_[0-7][0-9A-HJKMNP-TV-Z]{25}$/);
  assert.equal(credential.workstation_id, credential.device_id, "device_id must be canonical workstation identity");
  assert.match(credential.credential_id, /^cred_[0-9a-f]{32}$/);
  assert.match(credential.device_secret, /^devsec_[0-9a-f]{64}$/);
  assert.equal(JSON.stringify([...storage.map]).includes(credential.device_secret), false);

  const replay = await worker.fetch(post("/devices/pairings/consume", consumeBody), env);
  assert.equal(replay.status, 401);
  assert.equal((await replay.json()).code, "pairing_rejected");
});

test("recovery pairing rotates one active device credential without changing device identity", async () => {
  const { env } = makeEnv();
  const original = await pair(env, "recover-me");

  const before = await (await worker.fetch(get("/devices", "owner-secret"), env)).json();
  const create = await worker.fetch(post(
    "/devices/pairings",
    { ttl_seconds: 60, recover_device_id: original.device_id },
    "owner-secret",
  ), env);
  assert.equal(create.status, 200);
  const session = await create.json();

  const consume = await worker.fetch(post(
    "/devices/pairings/consume",
    { pairing_id: session.pairing_id, code: session.code },
  ), env);
  assert.equal(consume.status, 200);
  const recovered = await consume.json();
  assert.equal(recovered.device_id, original.device_id);
  assert.equal(recovered.workstation_id, original.workstation_id);
  assert.equal(recovered.recovered_existing, true);
  assert.notEqual(recovered.credential_id, original.credential_id);
  assert.notEqual(recovered.device_secret, original.device_secret);

  const after = await (await worker.fetch(get("/devices", "owner-secret"), env)).json();
  assert.equal(after.devices.length, before.devices.length);
  assert.equal(after.devices.find((device) => device.device_id === original.device_id)?.name, "recover-me");

  const oldCredential = await worker.fetch(postAsWorkstation(
    "/devices/pairings",
    { ttl_seconds: 60 },
    original.workstation_id,
    original.device_secret,
  ), env);
  assert.equal(oldCredential.status, 401);

  const newCredential = await worker.fetch(postAsWorkstation(
    "/devices/pairings",
    { ttl_seconds: 60 },
    recovered.workstation_id,
    recovered.device_secret,
  ), env);
  assert.equal(newCredential.status, 200);

  const replay = await worker.fetch(post(
    "/devices/pairings/consume",
    { pairing_id: session.pairing_id, code: session.code },
  ), env);
  assert.equal(replay.status, 401);
  assert.equal((await replay.json()).code, "pairing_rejected");
});

test("recovery pairing canonicalizes a legacy workstation identity without duplicating the device", async () => {
  const { env, registry } = makeEnv();
  const legacyConnect = await worker.fetch(
    wsRequest("prod-real-runtime", "legacy-secret", "legacy-recovery-air"),
    env,
  );
  assert.equal(legacyConnect.status, 200);

  const before = await (await registry.fetch(new Request("https://registry.internal/internal/devices"))).json();
  const legacy = before.devices.find((device) => device.workstation_id === "prod-real-runtime");
  assert.ok(legacy);
  assert.notEqual(legacy.device_id, legacy.workstation_id);

  const create = await worker.fetch(post(
    "/devices/pairings",
    { ttl_seconds: 60, recover_device_id: legacy.device_id },
    "owner-secret",
  ), env);
  assert.equal(create.status, 200);
  const session = await create.json();

  const consume = await worker.fetch(post(
    "/devices/pairings/consume",
    { pairing_id: session.pairing_id, code: session.code },
  ), env);
  assert.equal(consume.status, 200);
  const recovered = await consume.json();
  assert.equal(recovered.device_id, legacy.device_id);
  assert.equal(recovered.workstation_id, legacy.device_id);
  assert.equal(recovered.recovered_existing, true);

  const after = await (await registry.fetch(new Request("https://registry.internal/internal/devices"))).json();
  assert.equal(after.devices.length, before.devices.length);
  const canonical = after.devices.find((device) => device.device_id === legacy.device_id);
  assert.equal(canonical?.workstation_id, legacy.device_id);
  assert.equal(canonical?.name, "legacy-recovery-air");
  assert.equal(after.devices.some((device) => device.workstation_id === "prod-real-runtime"), false);

  const newCredential = await worker.fetch(postAsWorkstation(
    "/devices/pairings",
    { ttl_seconds: 60 },
    recovered.workstation_id,
    recovered.device_secret,
  ), env);
  assert.equal(newCredential.status, 200);

  const staleLegacy = await worker.fetch(postAsWorkstation(
    "/devices/pairings",
    { ttl_seconds: 60 },
    "prod-real-runtime",
    "legacy-secret",
  ), env);
  assert.equal(staleLegacy.status, 401);

  const finalInventory = await (await registry.fetch(new Request("https://registry.internal/internal/devices"))).json();
  assert.equal(finalInventory.devices.length, before.devices.length);
});

test("fleet-admin verifier rebind canonicalizes a legacy workstation identity", async () => {
  const { env, registry } = makeEnv();
  const legacyConnect = await worker.fetch(
    wsRequest("prod-real-runtime", "legacy-secret", "legacy-rebind-air"),
    env,
  );
  assert.equal(legacyConnect.status, 200);

  const before = await (await registry.fetch(new Request("https://registry.internal/internal/devices"))).json();
  const legacy = before.devices.find((device) => device.workstation_id === "prod-real-runtime");
  assert.ok(legacy);
  assert.notEqual(legacy.device_id, legacy.workstation_id);

  const replacementSecret = "replacement-device-secret";
  const verifier = createHash("sha256").update(replacementSecret).digest("hex");
  const rebind = await worker.fetch(post(
    "/devices/credential-rebind",
    { device_id: legacy.device_id, credential_verifier_sha256: verifier },
    "owner-secret",
  ), env);
  assert.equal(rebind.status, 200);

  const after = await (await registry.fetch(new Request("https://registry.internal/internal/devices"))).json();
  assert.equal(after.devices.length, before.devices.length);
  const canonical = after.devices.find((device) => device.device_id === legacy.device_id);
  assert.equal(canonical?.workstation_id, legacy.device_id);
  assert.equal(after.devices.some((device) => device.workstation_id === "prod-real-runtime"), false);

  const repairedCredential = await worker.fetch(postAsWorkstation(
    "/devices/pairings",
    { ttl_seconds: 60 },
    legacy.device_id,
    replacementSecret,
  ), env);
  assert.equal(repairedCredential.status, 200);

  const repairedOwner = await worker.fetch(postAsWorkstation(
    "/connectors/webchat-control",
    {},
    legacy.device_id,
    replacementSecret,
  ), env);
  assert.equal(repairedOwner.status, 400);
  assert.equal((await repairedOwner.json()).code, "invalid_webchat_control_grant");

  const staleLegacy = await worker.fetch(postAsWorkstation(
    "/devices/pairings",
    { ttl_seconds: 60 },
    "prod-real-runtime",
    "legacy-secret",
  ), env);
  assert.equal(staleLegacy.status, 401);
});

test("fleet-admin verifier rebind repairs one existing device without receiving its secret", async () => {
  const { env } = makeEnv();
  const original = await pair(env, "rebind-me");
  const replacementSecret = "devsec_ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";
  const verifier = createHash("sha256").update(replacementSecret).digest("hex");

  const before = await (await worker.fetch(get("/devices", "owner-secret"), env)).json();
  const rebind = await worker.fetch(post(
    "/devices/credential-rebind",
    { device_id: original.device_id, credential_verifier_sha256: verifier },
    "owner-secret",
  ), env);
  assert.equal(rebind.status, 200);
  const rebound = await rebind.json();
  assert.equal(rebound.device_id, original.device_id);
  assert.equal(JSON.stringify(rebound).includes(replacementSecret), false);
  assert.equal(JSON.stringify(rebound).includes("device_secret"), false);

  const after = await (await worker.fetch(get("/devices", "owner-secret"), env)).json();
  assert.equal(after.devices.length, before.devices.length);
  assert.equal(after.devices.find((device) => device.device_id === original.device_id)?.name, "rebind-me");

  const oldCredential = await worker.fetch(postAsWorkstation(
    "/devices/pairings",
    { ttl_seconds: 60 },
    original.workstation_id,
    original.device_secret,
  ), env);
  assert.equal(oldCredential.status, 401);

  const repairedCredential = await worker.fetch(postAsWorkstation(
    "/devices/pairings",
    { ttl_seconds: 60 },
    original.workstation_id,
    replacementSecret,
  ), env);
  assert.equal(repairedCredential.status, 200);

  const denied = await worker.fetch(post(
    "/devices/credential-rebind",
    { device_id: original.device_id, credential_verifier_sha256: verifier },
  ), env);
  assert.equal(denied.status, 401);
  assert.equal((await denied.json()).code, "credential_rebind_admin_required");
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
  const afterOwnAuth = forwarded.length;
  assert.ok(afterOwnAuth >= 1, "own authentication may seed its execution fence before websocket handoff");

  const impersonateB = await worker.fetch(wsRequest(b.workstation_id, a.device_secret), env);
  assert.equal(impersonateB.status, 401);
  assert.equal(forwarded.length, afterOwnAuth, "credential A must never add a call to workstation B");

  const legacyOnNewDevice = await worker.fetch(wsRequest(a.workstation_id, "legacy-secret"), env);
  assert.equal(legacyOnNewDevice.status, 401);
  assert.equal(forwarded.length, afterOwnAuth, "legacy global secret cannot add a call to a paired device");
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
  // Authentication may refresh the execution fence, then revoke must issue the
  // teardown kill-switch. The revoked credential never reaches a websocket handoff.
  assert.deepEqual(forwarded, [
    "/internal/execution-fence", // successful self-auth refresh
    "/internal/execution-fence", // revoke must fence execution before registry commit
    "/internal/revoke",          // then close/persist the historical kill-switch
  ]);
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

test("legacy shared-secret fleet approval is attributed to the canonical enrolled device", async () => {
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
      client_name: "Legacy Approver Test",
    }),
  }), h.env);
  const client = await registration.json();
  const authorize = new URL("https://edge.example/oauth/authorize");
  authorize.searchParams.set("client_id", client.client_id);
  authorize.searchParams.set("redirect_uri", "https://client.example/callback");
  authorize.searchParams.set("response_type", "code");
  authorize.searchParams.set("code_challenge", "B".repeat(43));
  authorize.searchParams.set("code_challenge_method", "S256");
  const page = await worker.fetch(new Request(authorize), h.env);
  const html = await page.text();
  const requestId = /const requestId="([A-Za-z0-9_-]+)";/.exec(html)?.[1];
  const approvalCode = /<div class="approval-code">(\d{6})<\/div>/.exec(html)?.[1];
  assert.ok(requestId && approvalCode);

  const approved = await worker.fetch(postAsWorkstation(
    "/connectors/approve",
    { request_id: requestId, code: approvalCode },
    "prod-real-runtime",
    "legacy-secret",
  ), h.env);
  assert.equal(approved.status, 200);

  const connector = [...oauthStorage.map.entries()]
    .find(([key]) => key.startsWith("connector:"))?.[1];
  assert.ok(connector);
  assert.match(connector.approved_by, /^device:dev_/);
  assert.doesNotMatch(connector.approved_by, /^legacy-link:/);
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
  assert.deepEqual(forwarded, [
    "/ws/prod-real-runtime",
    "/internal/execution-fence",
    "/internal/revoke",
  ]);
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

// #626: the public orphans route is scoped server-side to the authenticated owner device.
async function orphansHarness() {
  const h = makeEnv();
  const oauthStorage = new FakeStorage();
  const oauth = new OAuthStoreDO({ storage: oauthStorage }, { OAUTH_ISSUER: "https://edge.example" });
  h.env.OAUTH_STORE_DO = namespace(oauth);
  h.env.OAUTH_ISSUER = "https://edge.example";
  const legacyUnregistered = await worker.fetch(postAsWorkstation(
    "/connectors/page-assist/orphans", { action: "list" }, "prod-real-runtime", "legacy-secret",
  ), h.env);
  const connect = await worker.fetch(wsRequest("prod-real-runtime", "legacy-secret"), h.env);
  assert.equal(connect.status, 200);
  const list = await h.registry.fetch(new Request("https://registry.internal/internal/devices"));
  const deviceA = (await list.json()).devices.find((d) => d.workstation_id === "prod-real-runtime").device_id;
  const deviceB = (await pair(h.env, "orphans-b")).device_id;
  assert.match(deviceA, /^dev_/);
  assert.notEqual(deviceA, deviceB);
  const row = (connector_id, device_id, endpoint_ref) => ({ connector_id, grant_generation: 1, device_id, endpoint_ref });
  oauthStorage.map.set("grant:c1", {
    client_id: "c1", status: "active", principal_type: "connector", resource: "https://edge.example/mcp", scope: "mcp",
    approved_at_ms: 1000, approved_by: "device:owner", connector_id: "conn_new00000001", grant_generation: 1,
    page_assist: [row("conn_old00000001", deviceA, "epA"), row("conn_old00000001", deviceB, "epB")],
    webchat_control: [{ ...row("conn_old00000001", deviceB, "epB2"), provider: "chatgpt", account_ref: "*" }],
  });
  oauthStorage.writeCount = 0;
  const call = async (body) => {
    const resp = await worker.fetch(postAsWorkstation(
      "/connectors/page-assist/orphans", body, "prod-real-runtime", "legacy-secret",
    ), h.env);
    return { status: resp.status, body: await resp.json() };
  };
  return { oauthStorage, deviceA, deviceB, call, legacyUnregistered };
}

test("user Given orphans on owner device A and device B When A lists orphans without device_id Then only A's rows are returned and nothing is written", async () => {
  const h = await orphansHarness();
  const res = await h.call({ action: "list" });
  assert.equal(res.status, 200);
  assert.deepEqual(res.body.orphans.map((o) => o.endpoint_ref), ["epA"]);
  assert.ok(res.body.orphans.every((o) => o.device_id === h.deviceA));
  assert.equal(h.oauthStorage.writeCount, 0);
});

test("user Given owner device A authenticated When it asks for device B's orphans Then it gets 403 orphans_device_mismatch, none of B's rows, and zero writes", async () => {
  const h = await orphansHarness();
  const res = await h.call({ action: "list", device_id: h.deviceB });
  assert.equal(res.status, 403);
  assert.deepEqual(res.body, { ok: false, code: "orphans_device_mismatch" });
  assert.ok(!JSON.stringify(res.body).includes(h.deviceB));
  assert.equal(h.oauthStorage.writeCount, 0);
});

test("user Given owner device A authenticated When it lists orphans with its own device_id Then the request succeeds scoped to A", async () => {
  const h = await orphansHarness();
  const res = await h.call({ action: "list", device_id: h.deviceA });
  assert.equal(res.status, 200);
  assert.equal(res.body.ok, true);
  assert.deepEqual(res.body.orphans.map((o) => [o.device_id, o.endpoint_ref]), [[h.deviceA, "epA"]]);
  assert.equal(h.oauthStorage.writeCount, 0);
});

test("user Given owner auth When orphans are listed with a malformed device_id, an unknown key, no auth, or no registry device identity Then it fails closed and writes nothing", async () => {
  const h = await orphansHarness();
  const bad = await h.call({ action: "list", device_id: "dev_short" });
  assert.equal(bad.status, 400);
  assert.deepEqual(bad.body, { ok: false, code: "orphans_invalid_device_id" });
  const unknown = await h.call({ action: "list", extra: 1 });
  assert.equal(unknown.status, 400);
  assert.deepEqual(unknown.body, { ok: false, code: "bad_request" });
  assert.equal(h.legacyUnregistered.status, 403);
  assert.deepEqual(await h.legacyUnregistered.json(), { ok: false, code: "orphans_device_unresolved" });
  const generic = await worker.fetch(post("/connectors/page-assist/orphans", { action: "list" }, "owner-secret"), {
    ...makeEnv().env,
  });
  assert.equal(generic.status, 401);
  assert.equal(h.oauthStorage.writeCount, 0);
});

// #630: public dry-run/confirm route for page-assist grant migration.
const MIG_OLD = "conn_old00000001";
const MIG_NEW = "conn_new00000001";

async function migrateHarness(sharedRecord = false) {
  const h = makeEnv();
  const oauthStorage = new FakeStorage();
  const oauth = new OAuthStoreDO({ storage: oauthStorage }, { OAUTH_ISSUER: "https://edge.example" });
  h.env.OAUTH_STORE_DO = namespace(oauth);
  h.env.OAUTH_ISSUER = "https://edge.example";
  const legacyUnregistered = await worker.fetch(postAsWorkstation(
    "/connectors/page-assist/migrate",
    { from: MIG_OLD, to: MIG_NEW, plan_digest: "0".repeat(64), idempotency_key: "route-key-0000000001" },
    "prod-real-runtime", "legacy-secret",
  ), h.env);
  const connect = await worker.fetch(wsRequest("prod-real-runtime", "legacy-secret"), h.env);
  assert.equal(connect.status, 200);
  const list = await h.registry.fetch(new Request("https://registry.internal/internal/devices"));
  const deviceA = (await list.json()).devices.find((d) => d.workstation_id === "prod-real-runtime").device_id;
  const deviceB = (await pair(h.env, "migrate-b")).device_id;
  const row = (connector_id, device_id, endpoint_ref) => ({ connector_id, grant_generation: 1, device_id, endpoint_ref });
  oauthStorage.map.set("grant:c1", {
    client_id: "c1", status: "active", principal_type: "connector", resource: "https://edge.example/mcp", scope: "mcp",
    approved_at_ms: 1000, approved_by: "device:owner", connector_id: MIG_NEW, grant_generation: 1,
    page_assist: sharedRecord
      ? [row(MIG_OLD, deviceA, "epA"), row(MIG_OLD, deviceB, "epB"), row(MIG_NEW, deviceA, "epA2")]
      : [row(MIG_OLD, deviceA, "epA"), row(MIG_NEW, deviceA, "epA2")],
    webchat_control: [],
  });
  oauthStorage.writeCount = 0;
  const respond = async (resp) => ({ status: resp.status, body: await resp.json() });
  const dryRun = (body) => worker.fetch(postAsWorkstation(
    "/connectors/page-assist/orphans", body, "prod-real-runtime", "legacy-secret",
  ), h.env).then(respond);
  const confirm = (body) => worker.fetch(postAsWorkstation(
    "/connectors/page-assist/migrate", body, "prod-real-runtime", "legacy-secret",
  ), h.env).then(respond);
  return { oauthStorage, deviceA, deviceB, dryRun, confirm, legacyUnregistered };
}

test("user Given owner device A authenticated When it runs a migrate dry run through the public route Then the plan is scoped to A and carries a plan_digest", async () => {
  const h = await migrateHarness();
  const res = await h.dryRun({ action: "plan", from: MIG_OLD, to: MIG_NEW });
  assert.equal(res.status, 200, JSON.stringify(res.body));
  assert.equal(res.body.dry_run, true);
  assert.deepEqual(res.body.moves.map((m) => [m.from.device_id, m.from.endpoint_ref]), [[h.deviceA, "epA"]]);
  assert.match(res.body.plan_digest, /^[0-9a-f]{64}$/);
  assert.equal(h.oauthStorage.writeCount, 0);
});

test("user Given a dry-run digest from the public route When the owner confirms through the public route Then only A's rows move with audit and idempotency entries", async () => {
  const h = await migrateHarness();
  const plan = await h.dryRun({ action: "plan", from: MIG_OLD, to: MIG_NEW });
  assert.equal(plan.status, 200);
  const idem = "route-key-0000000002";
  const res = await h.confirm({ from: MIG_OLD, to: MIG_NEW, plan_digest: plan.body.plan_digest, idempotency_key: idem });
  assert.equal(res.status, 200, JSON.stringify(res.body));
  assert.equal(res.body.ok, true);
  assert.equal(res.body.action, "connector_page_assist_migrated");
  assert.equal(res.body.moved, 1);
  const stored = h.oauthStorage.map.get("grant:c1");
  assert.equal(stored.page_assist.find((r) => r.endpoint_ref === "epA").connector_id, MIG_NEW);
  assert.equal(stored.page_assist.find((r) => r.endpoint_ref === "epA2").connector_id, MIG_NEW);
  assert.equal(stored.audit.at(-1).by, `device:${h.deviceA}`);
  assert.ok(h.oauthStorage.map.has(`migrate-idem:v1:${h.deviceA}:${idem}`), "idempotency entry written");
  // Replay through the public route: same result, zero new writes.
  const writesAfterFirst = h.oauthStorage.writeCount;
  const replay = await h.confirm({ from: MIG_OLD, to: MIG_NEW, plan_digest: plan.body.plan_digest, idempotency_key: idem });
  assert.equal(replay.status, 200);
  assert.deepEqual(replay.body, res.body);
  assert.equal(h.oauthStorage.writeCount, writesAfterFirst, "replay writes nothing");
});

test("user Given a shared record holding device B rows under the same source When device A dry-runs or confirms through the public route Then both reject the whole plan with migrate_cross_device_rows and no grant or idempotency state changes", async () => {
  const h = await migrateHarness(true);
  const dry = await h.dryRun({ action: "plan", from: MIG_OLD, to: MIG_NEW });
  assert.equal(dry.status, 409);
  assert.equal(dry.body.code, "migrate_cross_device_rows");
  const res = await h.confirm({ from: MIG_OLD, to: MIG_NEW, plan_digest: "1".repeat(64), idempotency_key: "route-key-0000000003" });
  assert.equal(res.status, 409);
  assert.equal(res.body.code, "migrate_cross_device_rows");
  // #630 Q4: the rejection writes only the separate per-device reject ring.
  assert.equal(h.oauthStorage.writeCount, 1, "only the reject ring wrote");
  assert.deepEqual(
    h.oauthStorage.map.get(`migrate-reject-audit:v1:${h.deviceA}`).entries.map((entry) => [entry.code, entry.stage, entry.device_id]),
    [["migrate_cross_device_rows", "device_scope", h.deviceA]],
  );
  assert.equal([...h.oauthStorage.map.keys()].some((key) => key.startsWith("migrate-idem:")), false, "no idempotency entry for a reject");
  assert.equal(h.oauthStorage.map.get("grant:c1").page_assist.find((r) => r.endpoint_ref === "epB").connector_id, MIG_OLD);
});

test("user Given confirm calls without owner auth, an unresolved legacy device, unknown keys, or malformed fields When the public migrate route is called Then it fails closed with fixed codes, audits every authenticated shape reject, and keeps auth failures log-only", async () => {
  // #630 §7.6 "Rejected confirm (each stage)" + §7.8 Q4: every authenticated
  // rejection — including the Edge route's own shape rejects that never reach
  // the DO confirm transaction — is one slim per-device reject entry plus one
  // structured log line. Only 401/403 auth failures have no device to key on.
  const logs = [];
  const originalLog = console.log;
  console.log = (line) => logs.push(line);
  let h;
  let generic;
  let bad;
  let badDigest;
  let badKey;
  let tooLarge;
  try {
    h = await migrateHarness();
    generic = await worker.fetch(post("/connectors/page-assist/migrate", {
      from: MIG_OLD, to: MIG_NEW, plan_digest: "0".repeat(64), idempotency_key: "route-key-0000000004",
    }, "owner-secret"), { ...makeEnv().env });
    bad = await h.confirm({ from: MIG_OLD, to: MIG_NEW, plan_digest: "0".repeat(64), idempotency_key: "route-key-0000000004", extra: 1 });
    badDigest = await h.confirm({ from: MIG_OLD, to: MIG_NEW, plan_digest: "nothex", idempotency_key: "route-key-0000000005" });
    badKey = await h.confirm({ from: MIG_OLD, to: MIG_NEW, plan_digest: "0".repeat(64), idempotency_key: "short" });
    tooLarge = await h.confirm({ from: MIG_OLD, to: MIG_NEW, plan_digest: "0".repeat(64), idempotency_key: "route-key-0000000007", extra: "x".repeat(9000) });
  } finally {
    console.log = originalLog;
  }
  assert.equal(h.legacyUnregistered.status, 403);
  assert.deepEqual(await h.legacyUnregistered.json(), { ok: false, code: "migrate_device_unresolved" });
  assert.equal(generic.status, 401);
  assert.deepEqual(await generic.json(), { ok: false, code: "connector_owner_device_required" });
  assert.equal(bad.status, 400);
  assert.deepEqual(bad.body, { ok: false, code: "bad_request" });
  assert.equal(badDigest.status, 400);
  assert.deepEqual(badDigest.body, { ok: false, code: "migrate_invalid_plan_digest" });
  assert.equal(badKey.status, 400);
  assert.deepEqual(badKey.body, { ok: false, code: "migrate_idempotency_key_invalid" });
  assert.equal(tooLarge.status, 413);
  assert.deepEqual(tooLarge.body, { ok: false, code: "payload_too_large" });
  // Only the reject ring wrote: no grant or idempotency change at all.
  assert.equal(h.oauthStorage.writeCount, 4, "one reject-ring write per authenticated shape reject");
  assert.equal([...h.oauthStorage.map.keys()].some((key) => key.startsWith("migrate-idem:")), false, "no idempotency entry for a reject");
  assert.equal(h.oauthStorage.map.get("grant:c1").page_assist.find((r) => r.endpoint_ref === "epA").connector_id, MIG_OLD, "grant rows untouched");
  assert.deepEqual(
    h.oauthStorage.map.get(`migrate-reject-audit:v1:${h.deviceA}`).entries.map((entry) => [entry.code, entry.stage, entry.device_id]),
    [
      ["bad_request", "validate", h.deviceA],
      ["migrate_invalid_plan_digest", "validate", h.deviceA],
      ["migrate_idempotency_key_invalid", "validate", h.deviceA],
      ["payload_too_large", "validate", h.deviceA],
    ],
  );
  const rejectLines = logs.map((line) => JSON.parse(line)).filter((entry) => entry.event === "page_assist_migrate_rejected");
  assert.deepEqual(
    rejectLines.filter((line) => line.fields.device_id === h.deviceA).map((line) => [line.fields.code, line.fields.stage]),
    [
      ["bad_request", "validate"],
      ["migrate_invalid_plan_digest", "validate"],
      ["migrate_idempotency_key_invalid", "validate"],
      ["payload_too_large", "validate"],
    ],
  );
  // Auth failures: log line only, null device/digest/key, and no ring entry.
  const authLines = rejectLines.filter((line) => line.fields.device_id === null);
  assert.deepEqual(authLines.map((line) => line.fields.code).sort(), ["connector_owner_device_required", "migrate_device_unresolved"]);
  assert.deepEqual(authLines.map((line) => [line.fields.plan_digest, line.fields.idempotency_key_sha256_8]), [[null, null], [null, null]]);
  assert.equal(h.oauthStorage.map.has(`migrate-reject-audit:v1:${h.deviceB}`), false, "no ring for a device that never authenticated");
});

test("user Given owner device A authenticated When it runs a migrate dry run asserting its own device_id Then the plan is device-scoped and succeeds", async () => {
  const h = await migrateHarness();
  const res = await h.dryRun({ action: "plan", from: MIG_OLD, to: MIG_NEW, device_id: h.deviceA });
  assert.equal(res.status, 200, JSON.stringify(res.body));
  assert.equal(res.body.dry_run, true);
  assert.deepEqual(res.body.moves.map((m) => m.from.endpoint_ref), ["epA"]);
  assert.ok(res.body.moves.every((m) => m.from.device_id === h.deviceA));
  assert.match(res.body.plan_digest, /^[0-9a-f]{64}$/);
  // The assertion must match: B's device_id is refused with the fixed mismatch code.
  const mismatch = await h.dryRun({ action: "plan", from: MIG_OLD, to: MIG_NEW, device_id: h.deviceB });
  assert.equal(mismatch.status, 403);
  assert.deepEqual(mismatch.body, { ok: false, code: "orphans_device_mismatch" });
  assert.equal(h.oauthStorage.writeCount, 0);
});

test("user Given a migrate request where from equals to When it passes the public route validation Then both layers agree on migrate_source_is_current", async () => {
  const h = await migrateHarness();
  const same = await h.confirm({ from: MIG_NEW, to: MIG_NEW, plan_digest: "0".repeat(64), idempotency_key: "route-key-0000000006" });
  assert.equal(same.status, 400);
  assert.deepEqual(same.body, { ok: false, code: "migrate_source_is_current" }, "public layer returns the DO's code");
  // Malformed ids keep their own fixed code.
  const malformed = await h.confirm({ from: "bogus", to: MIG_NEW, plan_digest: "0".repeat(64), idempotency_key: "route-key-0000000007" });
  assert.equal(malformed.status, 400);
  assert.deepEqual(malformed.body, { ok: false, code: "migrate_invalid_connector_id" });
  // #630 Q4: both authenticated shape rejects are audited; nothing else wrote.
  assert.equal(h.oauthStorage.writeCount, 2, "one reject-ring write per authenticated shape reject");
  assert.deepEqual(
    h.oauthStorage.map.get(`migrate-reject-audit:v1:${h.deviceA}`).entries.map((entry) => [entry.code, entry.stage]),
    [["migrate_source_is_current", "validate"], ["migrate_invalid_connector_id", "validate"]],
  );
  assert.equal(h.oauthStorage.map.get("grant:c1").page_assist.find((r) => r.endpoint_ref === "epA").connector_id, MIG_OLD, "grant rows untouched");
});

async function approveConnector(env, clientName) {
  const registration = await worker.fetch(new Request("https://edge.example/oauth/register", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      redirect_uris: ["https://client.example/callback"],
      token_endpoint_auth_method: "none",
      client_name: clientName,
    }),
  }), env);
  assert.equal(registration.status, 201);
  const client = await registration.json();
  const authorize = new URL("https://edge.example/oauth/authorize");
  authorize.searchParams.set("client_id", client.client_id);
  authorize.searchParams.set("redirect_uri", "https://client.example/callback");
  authorize.searchParams.set("response_type", "code");
  authorize.searchParams.set("code_challenge", "A".repeat(43));
  authorize.searchParams.set("code_challenge_method", "S256");
  authorize.searchParams.set("state", `state-${clientName}`);
  const pendingResponse = await worker.fetch(new Request(authorize), env);
  assert.equal(pendingResponse.status, 200);
  const html = await pendingResponse.text();
  const requestId = /const requestId="([A-Za-z0-9_-]+)";/.exec(html)?.[1];
  const approvalCode = /<div class="approval-code">(\d{6})<\/div>/.exec(html)?.[1];
  assert.ok(requestId && approvalCode);
  const approved = await worker.fetch(post("/connectors/approve", {
    request_id: requestId,
    code: approvalCode,
  }, "owner-secret"), env);
  assert.equal(approved.status, 200);
  const inventory = await worker.fetch(new Request("https://edge.example/connectors", {
    headers: { authorization: "Bearer owner-secret" },
  }), env);
  assert.equal(inventory.status, 200);
  const body = await inventory.json();
  const connector = body.connectors.find((row) => row.client_id === client.client_id);
  assert.ok(connector);
  return connector;
}

function ownerWebChatGrant(env, body, workstationId = "prod-real-runtime", secret = "legacy-secret") {
  return worker.fetch(postAsWorkstation(
    "/connectors/webchat-control",
    body,
    workstationId,
    secret,
  ), env);
}

test("user owner labels a connector | Given an approved connector whose client_name is not an account | When the owner sets and clears platform_account | Then the label is display metadata and grant_generation stays unchanged", async () => {
  const h = makeEnv();
  const oauth = new OAuthStoreDO({ storage: new FakeStorage() }, { OAUTH_ISSUER: "https://edge.example" });
  h.env.OAUTH_STORE_DO = namespace(oauth);
  h.env.OAUTH_ISSUER = "https://edge.example";

  const cursorNamed = await approveConnector(h.env, "Cursor");
  assert.equal(cursorNamed.platform_account, null);
  assert.equal(cursorNamed.client_name, "Cursor");
  const before = cursorNamed.grant_generation;

  const spoofed = await approveConnector(h.env, "grokbot-whshang@gmail.com");
  assert.equal(spoofed.platform_account, null, "client-controlled client_name is never auto-labeled as a platform account");

  const labeled = await worker.fetch(post("/connectors/label", {
    connector_id: cursorNamed.connector_id,
    platform_account: "chatgpt-whshang1@gmail.com",
  }, "owner-secret"), h.env);
  assert.equal(labeled.status, 200);
  const labeledBody = await labeled.json();
  assert.equal(labeledBody.platform_account, "chatgpt-whshang1@gmail.com");
  assert.equal(labeledBody.grant_generation, before);

  const rejected = await worker.fetch(post("/connectors/label", {
    connector_id: cursorNamed.connector_id,
    platform_account: "Cursor",
  }, "owner-secret"), h.env);
  assert.equal(rejected.status, 400);
  assert.equal((await rejected.json()).code, "invalid_platform_account");

  const cleared = await worker.fetch(post("/connectors/label", {
    connector_id: cursorNamed.connector_id,
    platform_account: null,
  }, "owner-secret"), h.env);
  assert.equal(cleared.status, 200);
  const clearedBody = await cleared.json();
  assert.equal(clearedBody.platform_account, null);
  assert.equal(clearedBody.grant_generation, before);
});

test("user owner grants WebChat control across the fleet | Given an owner device, another device in the same fleet, a non-owner device, and an unenrolled device | When device-wide allow and deny are requested | Then same-fleet and self grants succeed and cross-owner or non-owner requests are denied", async () => {
  const h = makeEnv();
  const oauth = new OAuthStoreDO({ storage: new FakeStorage() }, { OAUTH_ISSUER: "https://edge.example" });
  h.env.OAUTH_STORE_DO = namespace(oauth);
  h.env.OAUTH_ISSUER = "https://edge.example";
  const connector = await approveConnector(h.env, "Cursor");
  // The owner is a genuinely enrolled device with its own credential:
  // device-wide grants require credential authentication (never the legacy
  // shared secret), so the default workstation becomes the paired owner.
  const owner = await pair(h.env, "fleet-owner");
  h.env.DEFAULT_WORKSTATION_ID = owner.workstation_id;
  const ownerDeviceId = owner.device_id;
  const other = await pair(h.env, "same-fleet-other");
  const logs = [];
  const originalLog = console.log;
  console.log = (line) => logs.push(line);
  try {
    const realStoreNamespace = h.env.OAUTH_STORE_DO;
    h.env.OAUTH_STORE_DO = namespace({
      fetch(request) {
        if (new URL(request.url).pathname === "/internal/oauth/grant/webchat-control") {
          return new Response(JSON.stringify({ ok: false, code: "storage_unavailable" }), { status: 503 });
        }
        return oauth.fetch(request);
      },
    });
    try {
      const failed = await ownerWebChatGrant(h.env, {
        connector_id: connector.connector_id,
        device_id: other.device_id,
        scope: "device",
        allowed: true,
      }, owner.workstation_id, owner.device_secret);
      assert.equal(failed.status, 409);
      assert.equal(logs.some((line) => {
        try { return JSON.parse(line).event === "connector.webchat_control_grant"; } catch { return false; }
      }), false, "failed storage writes cannot emit a successful grant audit");
    } finally {
      h.env.OAUTH_STORE_DO = realStoreNamespace;
    }

    const sameFleet = await ownerWebChatGrant(h.env, {
      connector_id: connector.connector_id,
      device_id: other.device_id,
      scope: "device",
      allowed: true,
    }, owner.workstation_id, owner.device_secret);
    assert.equal(sameFleet.status, 200);
    const sameFleetBody = await sameFleet.json();
    assert.equal(sameFleetBody.scope, "device");
    assert.equal(sameFleetBody.requester_device_id, ownerDeviceId);
    assert.equal(sameFleetBody.device_id, other.device_id);
    assert.deepEqual(sameFleetBody.grants, [{
      connector_id: connector.connector_id,
      grant_generation: connector.grant_generation,
      device_id: other.device_id,
      endpoint_ref: "*",
      provider: "*",
      account_ref: "*",
    }]);

    const selfGrant = await ownerWebChatGrant(h.env, {
      connector_id: connector.connector_id,
      device_id: ownerDeviceId,
      scope: "device",
      allowed: true,
    }, owner.workstation_id, owner.device_secret);
    assert.equal(selfGrant.status, 200);
    const selfBody = await selfGrant.json();
    assert.equal(selfBody.device_id, ownerDeviceId);
    assert.ok(selfBody.grants.some((grant) => grant.device_id === ownerDeviceId && grant.endpoint_ref === "*"));

    const otherOwner = await ownerWebChatGrant(h.env, {
      connector_id: connector.connector_id,
      device_id: "dev_01M1TEST000000000000000000",
      scope: "device",
      allowed: true,
    }, owner.workstation_id, owner.device_secret);
    assert.equal(otherOwner.status, 403);
    assert.equal((await otherOwner.json()).code, "connector_owner_device_mismatch");

    const nonOwner = await worker.fetch(postAsWorkstation(
      "/connectors/webchat-control",
      {
        connector_id: connector.connector_id,
        device_id: other.device_id,
        scope: "device",
        allowed: true,
      },
      other.workstation_id,
      other.device_secret,
    ), h.env);
    assert.equal(nonOwner.status, 401);
    assert.equal((await nonOwner.json()).code, "connector_owner_device_required");

    const mixed = await ownerWebChatGrant(h.env, {
      connector_id: connector.connector_id,
      device_id: other.device_id,
      scope: "device",
      endpoint_ref: "*",
      allowed: true,
    }, owner.workstation_id, owner.device_secret);
    assert.equal(mixed.status, 400);
    assert.equal((await mixed.json()).code, "invalid_webchat_control_grant");

    const denied = await ownerWebChatGrant(h.env, {
      connector_id: connector.connector_id,
      device_id: other.device_id,
      scope: "device",
      allowed: false,
    }, owner.workstation_id, owner.device_secret);
    assert.equal(denied.status, 200);
    const deniedBody = await denied.json();
    assert.equal(deniedBody.allowed, false);
    assert.equal(deniedBody.grants.some((grant) => grant.device_id === other.device_id && grant.endpoint_ref === "*"), false);
  } finally {
    console.log = originalLog;
  }
  const events = logs.flatMap((line) => {
    try { return [JSON.parse(line)]; } catch { return []; }
  });
  const granted = events.find((entry) => entry.event === "connector.webchat_control_grant"
    && entry.fields?.targetDeviceId === other.device_id
    && entry.fields?.allowed === true);
  assert.equal(granted.fields.requesterDeviceId, ownerDeviceId);
  assert.equal(granted.fields.connectorId, connector.connector_id);
  const mismatch = events.find((entry) => entry.event === "connector.webchat_control_denied"
    && entry.fields?.code === "connector_owner_device_mismatch");
  assert.equal(mismatch.fields.requesterDeviceId, ownerDeviceId);
  assert.equal(mismatch.fields.targetDeviceId, "dev_01M1TEST000000000000000000");
  assert.equal(mismatch.fields.connectorId, connector.connector_id);
});

test("user cannot widen Page Assist or grant to non-active targets | Given an owner device with a credential, an active connector, and a suspended fleet device | When wildcard Page Assist or device-wide grants to suspended/unknown targets are requested | Then each request is rejected fail-closed", async () => {
  const h = makeEnv();
  const oauth = new OAuthStoreDO({ storage: new FakeStorage() }, { OAUTH_ISSUER: "https://edge.example" });
  h.env.OAUTH_STORE_DO = namespace(oauth);
  h.env.OAUTH_ISSUER = "https://edge.example";
  const connector = await approveConnector(h.env, "Cursor");
  const owner = await pair(h.env, "strict-owner");
  h.env.DEFAULT_WORKSTATION_ID = owner.workstation_id;
  const suspended = await pair(h.env, "suspended-target");
  const suspendedRecord = h.storage.map.get(`device:${suspended.device_id}`);
  h.storage.map.set(`device:${suspended.device_id}`, { ...suspendedRecord, authorization: "suspended" });

  const wildcardPageAssist = await worker.fetch(postAsWorkstation(
    "/connectors/page-assist",
    {
      connector_id: connector.connector_id,
      device_id: owner.device_id,
      endpoint_ref: "*",
      allowed: true,
    },
    owner.workstation_id,
    owner.device_secret,
  ), h.env);
  assert.equal(wildcardPageAssist.status, 400);
  assert.equal((await wildcardPageAssist.json()).code, "invalid_page_assist_grant");

  const legacyWildcardShape = await ownerWebChatGrant(h.env, {
    connector_id: connector.connector_id,
    device_id: owner.device_id,
    endpoint_ref: "*",
    provider: "*",
    account_ref: "*",
    allowed: true,
  }, owner.workstation_id, owner.device_secret);
  assert.equal(legacyWildcardShape.status, 200, "credential owner may still issue device scope via the legacy wire shape");

  const suspendedTarget = await ownerWebChatGrant(h.env, {
    connector_id: connector.connector_id,
    device_id: suspended.device_id,
    scope: "device",
    allowed: true,
  }, owner.workstation_id, owner.device_secret);
  assert.equal(suspendedTarget.status, 403);
  assert.equal((await suspendedTarget.json()).code, "connector_target_device_suspended");

  const unknownTarget = await ownerWebChatGrant(h.env, {
    connector_id: connector.connector_id,
    device_id: "dev_01M1TEST000000000000000000",
    scope: "device",
    allowed: true,
  }, owner.workstation_id, owner.device_secret);
  assert.equal(unknownTarget.status, 403);
  assert.equal((await unknownTarget.json()).code, "connector_owner_device_mismatch");
});

test("user cannot hijack connector labels | Given a legacy default owner, a credential owner device, a non-owner enrolled device, and an operator bearer | When labels are written | Then only owner credential and operator succeed and failed writes leave the record unchanged", async () => {
  const h = makeEnv();
  const oauth = new OAuthStoreDO({ storage: new FakeStorage() }, { OAUTH_ISSUER: "https://edge.example" });
  h.env.OAUTH_STORE_DO = namespace(oauth);
  h.env.OAUTH_ISSUER = "https://edge.example";
  const connector = await approveConnector(h.env, "Cursor");
  const owner = await pair(h.env, "label-owner");
  const other = await pair(h.env, "label-nonowner");

  // Legacy shared-secret default owner is rejected even though it is the
  // configured default workstation (registry correlation is not a credential).
  const legacy = await worker.fetch(postAsWorkstation(
    "/connectors/label",
    { connector_id: connector.connector_id, platform_account: "legacy-owner@example.com" },
    "prod-real-runtime", "legacy-secret",
  ), h.env);
  assert.equal(legacy.status, 403);
  assert.equal((await legacy.json()).code, "connector_owner_device_mismatch");

  // A wrong bearer has no credential at all.
  const wrongBearer = await worker.fetch(post(
    "/connectors/label", { connector_id: connector.connector_id, platform_account: "x-owner@example.com" }, "wrong-secret",
  ), h.env);
  assert.equal(wrongBearer.status, 401);
  assert.equal((await wrongBearer.json()).code, "connector_owner_device_required");

  // The enrolled owner device credential writes the label.
  h.env.DEFAULT_WORKSTATION_ID = owner.workstation_id;
  const ownerLabel = await worker.fetch(postAsWorkstation(
    "/connectors/label",
    { connector_id: connector.connector_id, platform_account: "grokbot-owner@example.com" },
    owner.workstation_id, owner.device_secret,
  ), h.env);
  assert.equal(ownerLabel.status, 200);
  const ownerLabelBody = await ownerLabel.json();
  assert.equal(ownerLabelBody.platform_account, "grokbot-owner@example.com");
  assert.equal(ownerLabelBody.grant_generation, connector.grant_generation, "labels never alter grant generation");

  // A non-owner active enrolled device cannot relabel or redirect selection.
  const nonOwner = await worker.fetch(postAsWorkstation(
    "/connectors/label",
    { connector_id: connector.connector_id, platform_account: "attacker-owner@example.com" },
    other.workstation_id, other.device_secret,
  ), h.env);
  assert.equal(nonOwner.status, 403);
  assert.equal((await nonOwner.json()).code, "connector_owner_device_mismatch");

  // The operator bearer keeps its contract.
  const operator = await worker.fetch(post(
    "/connectors/label", { connector_id: connector.connector_id, platform_account: "chatgpt-owner@example.com" }, "owner-secret",
  ), h.env);
  assert.equal(operator.status, 200);

  // No failed relabel redirected the record: the label is the operator's
  // last successful write, on the same connector and generation.
  const inventory = await worker.fetch(new Request("https://edge.example/connectors", {
    headers: { authorization: "Bearer owner-secret" },
  }), h.env);
  const rows = (await inventory.json()).connectors.filter((row) => row.connector_id === connector.connector_id);
  assert.equal(rows.length, 1);
  assert.equal(rows[0].platform_account, "chatgpt-owner@example.com");
  assert.equal(rows[0].grant_generation, connector.grant_generation);
});

test("user owner may revoke grants on suspended or revoked same-fleet targets | Given a credential owner and a device-wide grant on an active target | When the target is suspended or revoked and the grant is denied | Then the stored grant is deleted and never resurfaces after reactivation", async () => {
  const h = makeEnv();
  const oauth = new OAuthStoreDO({ storage: new FakeStorage() }, { OAUTH_ISSUER: "https://edge.example" });
  h.env.OAUTH_STORE_DO = namespace(oauth);
  h.env.OAUTH_ISSUER = "https://edge.example";
  const connector = await approveConnector(h.env, "Cursor");
  const owner = await pair(h.env, "revoke-owner");
  h.env.DEFAULT_WORKSTATION_ID = owner.workstation_id;
  const target = await pair(h.env, "revoke-target");

  const grant = { connector_id: connector.connector_id, device_id: target.device_id, scope: "device" };
  const allowActive = await ownerWebChatGrant(h.env, { ...grant, allowed: true }, owner.workstation_id, owner.device_secret);
  assert.equal(allowActive.status, 200, "allow on an ACTIVE target succeeds");

  const record = h.storage.map.get(`device:${target.device_id}`);
  h.storage.map.set(`device:${target.device_id}`, { ...record, authorization: "suspended" });
  const allowSuspended = await ownerWebChatGrant(h.env, { ...grant, allowed: true }, owner.workstation_id, owner.device_secret);
  assert.equal(allowSuspended.status, 403, "allow on a suspended target always fails");
  assert.equal((await allowSuspended.json()).code, "connector_target_device_suspended");

  const denySuspended = await ownerWebChatGrant(h.env, { ...grant, allowed: false }, owner.workstation_id, owner.device_secret);
  assert.equal(denySuspended.status, 200, "a credential owner may deny a suspended same-fleet target");
  assert.equal((await denySuspended.json()).grants.some((g) => g.device_id === target.device_id), false,
    "the stored device-wide grant is deleted");

  // Reactivation must not resurrect the stale grant.
  h.storage.map.set(`device:${target.device_id}`, { ...record, authorization: "active" });
  const stored = await oauth.fetch(new Request("https://oauth.internal/internal/oauth/grant/get", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ client_id: connector.client_id }),
  }));
  assert.equal(stored.status, 200);
  const storedBody = await stored.json();
  assert.equal(storedBody.record.webchat_control.some((g) => g.device_id === target.device_id), false,
    "reenabling the device does not restore the revoked grant");

  // A revoked target may also be denied, but never granted.
  h.storage.map.set(`device:${target.device_id}`, {
    ...record,
    authorization: "revoked",
    revoked_at_ms: (record.updated_at_ms ?? 0) + 1,
  });
  const allowRevoked = await ownerWebChatGrant(h.env, { ...grant, allowed: true }, owner.workstation_id, owner.device_secret);
  assert.equal(allowRevoked.status, 403, "allow on a revoked target always fails");
  assert.equal((await allowRevoked.json()).code, "connector_target_device_revoked");
  const denyRevoked = await ownerWebChatGrant(h.env, { ...grant, allowed: false }, owner.workstation_id, owner.device_secret);
  assert.equal(denyRevoked.status, 200, "a credential owner may deny a revoked same-fleet target");

  // Reactivate so the remaining checks run against an ACTIVE target again.
  h.storage.map.set(`device:${target.device_id}`, { ...record, authorization: "active" });
  // An absent target is never blindly denied: no write happens for unknown devices.
  const endpointGrant = await ownerWebChatGrant(h.env, {
    connector_id: connector.connector_id,
    device_id: target.device_id,
    endpoint_ref: `be_${"h".repeat(64)}`,
    provider: "chatgpt",
    allowed: true,
  }, owner.workstation_id, owner.device_secret);
  assert.equal(endpointGrant.status, 200);
  const denyUnknown = await ownerWebChatGrant(h.env, {
    connector_id: connector.connector_id,
    device_id: "dev_01M1TEST000000000000000000",
    scope: "device",
    allowed: false,
  }, owner.workstation_id, owner.device_secret);
  assert.equal(denyUnknown.status, 403, "deny for an unknown target is rejected");
  assert.equal((await denyUnknown.json()).code, "connector_owner_device_mismatch");
  const afterUnknown = await oauth.fetch(new Request("https://oauth.internal/internal/oauth/grant/get", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ client_id: connector.client_id }),
  }));
  const afterUnknownBody = (await afterUnknown.json());
  assert.equal(afterUnknownBody.record.webchat_control.some((g) => g.device_id === target.device_id), true,
    "the rejected deny wrote nothing");
});

test("user legacy owner with a registry device id still cannot widen device scope | Given the default workstation on the legacy shared secret with one registered device | When device-wide allow or suspended-target deny is requested | Then every device-scope write is rejected fail-closed", async () => {
  const h = makeEnv();
  const oauth = new OAuthStoreDO({ storage: new FakeStorage() }, { OAUTH_ISSUER: "https://edge.example" });
  h.env.OAUTH_STORE_DO = namespace(oauth);
  h.env.OAUTH_ISSUER = "https://edge.example";
  const connector = await approveConnector(h.env, "Cursor");
  const ensured = await h.registry.fetch(new Request("https://registry.internal/internal/devices/legacy/ensure", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ workstation_id: "prod-real-runtime", name: "legacy-owner" }),
  }));
  assert.equal(ensured.status, 200);
  const legacyDeviceId = (await ensured.json()).device.device_id;
  assert.ok(legacyDeviceId, "the legacy workstation has a registry device correlation");
  const target = await pair(h.env, "legacy-gap-target");

  const wide = { connector_id: connector.connector_id, device_id: target.device_id, scope: "device", allowed: true };
  const allow = await ownerWebChatGrant(h.env, wide, "prod-real-runtime", "legacy-secret");
  assert.equal(allow.status, 403,
    "a legacy shared-secret owner with a resolvable registry device id still cannot issue device scope");

  const endpoint = await ownerWebChatGrant(h.env, {
    connector_id: connector.connector_id,
    device_id: target.device_id,
    endpoint_ref: `be_${"k".repeat(64)}`,
    provider: "chatgpt",
    allowed: true,
  }, "prod-real-runtime", "legacy-secret");
  assert.equal(endpoint.status, 200, "endpoint-scope grants keep their current legacy contract");

  const record = h.storage.map.get(`device:${target.device_id}`);
  h.storage.map.set(`device:${target.device_id}`, { ...record, authorization: "suspended" });
  const deny = await ownerWebChatGrant(h.env, { ...wide, allowed: false }, "prod-real-runtime", "legacy-secret");
  assert.equal(deny.status, 403,
    "suspended-target deny is a device-scope write and stays credential-only");
});
