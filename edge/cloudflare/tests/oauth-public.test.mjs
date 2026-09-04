import { test } from "node:test";
import assert from "node:assert/strict";
import {
  handleOAuthPublic,
  createOAuthPublicStore,
} from "../dist/oauth-public.js";
import { OAuthStoreDO } from "../dist/oauth-store-do.js";
import {
  createOAuthIdentity,
  createRs256AccessTokenVerifier,
  hashOAuthApprovalCode,
  hashOpaqueToken,
  oauthEdgeMetadata,
  protectedResourceEdgeMetadata,
} from "../dist/oauth-edge.js";

// ---------------------------------------------------------------------------
// Harness: a REAL OAuthStoreDO over an in-memory StorageMock. The handler
// drives it through createOAuthPublicStore(stub), which proxies to the DO's
// internal HTTP API — no real files, no secondary state, no network.
// ---------------------------------------------------------------------------

class StorageMock {
  map = new Map();
  async get(key) { return this.map.get(key); }
  async put(key, value) { this.map.set(key, structuredClone(value)); }
  async delete(key) { return this.map.delete(key); }
  async list(opts = {}) {
    const out = new Map();
    const prefix = opts.prefix ?? "";
    for (const [k, v] of this.map) if (k.startsWith(prefix)) out.set(k, structuredClone(v));
    return out;
  }
  async transaction(fn) { return fn(this); }
}

const ISSUER = "https://herdr-mcp.example.com";
const IDENTITY = createOAuthIdentity(ISSUER);
const NOW_MS = 1_700_000_000_000;
const NOW_SEC = Math.floor(NOW_MS / 1000);
const APPROVAL_SECRET = "test-fleet-approval-secret-not-for-production";

function makeOptions(overrides = {}) {
  const storage = new StorageMock();
  const instance = new OAuthStoreDO({ storage }, { OAUTH_ISSUER: ISSUER });
  const stub = { fetch: (r) => instance.fetch(r) };
  const store = createOAuthPublicStore(stub);
  return {
    identity: IDENTITY,
    store,
    approvalSecret: APPROVAL_SECRET,
    nowMs: () => NOW_MS,
    serverName: "herdr-mcp",
    serverVersion: "0.3.26",
    __do: instance, // test-only access to the DO signing endpoint
    ...overrides,
  };
}

// --- request builders -------------------------------------------------------

function GET(path, opts) {
  return handleOAuthPublic(new Request(`https://x.example${path}`), opts);
}

function POST(path, body, opts, { form = false, headers = {} } = {}) {
  const init = { method: "POST", headers: { ...headers } };
  if (form) {
    init.headers["content-type"] = "application/x-www-form-urlencoded";
    init.body = new URLSearchParams(body).toString();
  } else {
    init.headers["content-type"] = "application/json";
    init.body = JSON.stringify(body);
  }
  return handleOAuthPublic(new Request(`https://x.example${path}`, init), opts);
}

// --- crypto helpers ---------------------------------------------------------

const enc = new TextEncoder();

function b64url(bytes) {
  let s = "";
  for (const b of bytes) s += String.fromCharCode(b);
  return btoa(s).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

async function sha256Hex(s) {
  const d = await crypto.subtle.digest("SHA-256", enc.encode(s));
  return Array.from(new Uint8Array(d)).map((b) => b.toString(16).padStart(2, "0")).join("");
}

async function s256Challenge(verifier) {
  if (!/^[A-Za-z0-9\-._~]{43,128}$/.test(verifier)) throw new Error("bad verifier");
  const d = await crypto.subtle.digest("SHA-256", enc.encode(verifier));
  return b64url(new Uint8Array(d));
}

/** DCR a client, return { client_id, client_secret }. */
async function registerClient(opts, overrides = {}) {
  const resp = await POST("/oauth/register", {
    redirect_uris: ["https://app.example/cb"],
    token_endpoint_auth_method: "client_secret_post",
    ...overrides,
  }, opts);
  assert.equal(resp.status, 201);
  return resp.json();
}

async function pendingAuthorization(opts, client_id, redirect_uri = "https://app.example/cb", state = "st") {
  const verifier = "E".repeat(43) + "zZ-._";
  const challenge = await s256Challenge(verifier);
  const qs = new URLSearchParams({
    client_id, redirect_uri, response_type: "code",
    code_challenge: challenge, code_challenge_method: "S256", state,
  });
  const resp = await GET(`/oauth/authorize?${qs}`, opts);
  assert.equal(resp.status, 200, "unapproved connector should receive the approval page, not an OAuth code");
  assert.match(resp.headers.get("content-type") ?? "", /^text\/html/);
  const html = await resp.text();
  const requestId = /const requestId="([A-Za-z0-9_-]+)";/.exec(html)?.[1];
  const resumeToken = /const resumeToken="([A-Za-z0-9_-]+)";/.exec(html)?.[1];
  const approvalCode = /<p class="code">(\d{6})<\/p>/.exec(html)?.[1];
  assert.ok(requestId, "approval page should expose request id");
  assert.ok(resumeToken, "approval page should carry an opaque resume token for same-page polling");
  assert.ok(approvalCode, "approval page should expose the one-time six-digit code");
  assert.ok(html.includes(`herdr-mcp connector approve ${requestId}`));
  assert.match(html, /any computer already enrolled in this Herdr Worker/);
  assert.match(html, /explicitly approved by this Worker/);
  assert.match(resp.headers.get("content-security-policy") ?? "", /frame-ancestors 'none'/);
  assert.equal(resp.headers.get("x-frame-options"), "DENY");
  return { requestId, resumeToken, approvalCode, verifier, challenge, state };
}

async function approvePending(opts, pending, approver = "device:dev_owner") {
  const approved = await opts.store.approveApproval(
    pending.requestId,
    await hashOAuthApprovalCode(APPROVAL_SECRET, pending.requestId, pending.approvalCode),
    approver,
    NOW_MS,
  );
  assert.equal(approved.ok, true, "fleet approval should succeed");
  const poll = await GET(`/oauth/authorize/poll?${new URLSearchParams({
    request_id: pending.requestId,
    resume_token: pending.resumeToken,
  })}`, opts);
  assert.equal(poll.status, 200);
  const body = await poll.json();
  assert.equal(body.status, "approved");
  const loc = new URL(body.redirect);
  return { code: loc.searchParams.get("code"), location: loc };
}

/** Authorize for a client through the explicit fleet-approval flow. */
async function makeAuthCode(opts, client_id, redirect_uri = "https://app.example/cb") {
  const grant = await opts.store.getGrant(client_id);
  if (grant?.status === "active") {
    const verifier = "E".repeat(43) + "zZ-._";
    const challenge = await s256Challenge(verifier);
    const qs = new URLSearchParams({
      client_id, redirect_uri, response_type: "code",
      code_challenge: challenge, code_challenge_method: "S256", state: "st",
    });
    const resp = await GET(`/oauth/authorize?${qs}`, opts);
    assert.equal(resp.status, 302, "an already approved active connector grant should not require approval again");
    return { code: new URL(resp.headers.get("location")).searchParams.get("code"), verifier };
  }
  const pending = await pendingAuthorization(opts, client_id, redirect_uri);
  const approved = await approvePending(opts, pending);
  return { code: approved.code, verifier: pending.verifier };
}

/** Exchange an auth code; body may be partially overridden. */
function tokenBody(client_id, code, verifier, extra = {}) {
  return {
    grant_type: "authorization_code",
    code,
    redirect_uri: "https://app.example/cb",
    client_id,
    code_verifier: verifier,
    resource: IDENTITY.resource,
    ...extra,
  };
}

/** Import the DO's signing public JWK and build a verifier. */
async function doVerifier(opts) {
  const resp = await opts.__do.fetch(new Request("https://do.internal/internal/oauth/signing/ensure", { method: "POST", body: "{}" }));
  const { public_jwk } = await resp.json();
  const key = await crypto.subtle.importKey(
    "jwk", public_jwk, { name: "RSASSA-PKCS1-v1_5", hash: "SHA-256" }, false, ["verify"],
  );
  return createRs256AccessTokenVerifier(IDENTITY, key);
}

// ---------------------------------------------------------------------------
// 1. CORS / OPTIONS
// ---------------------------------------------------------------------------

test("OPTIONS on any owned path returns 204 with exact CORS headers", async () => {
  const opts = makeOptions();
  for (const path of ["/.well-known/oauth-authorization-server", "/oauth/register", "/oauth/token", "/.well-known/mcp.json"]) {
    const resp = await handleOAuthPublic(new Request(`https://x.example${path}`, { method: "OPTIONS" }), opts);
    assert.equal(resp.status, 204);
    assert.equal(resp.headers.get("access-control-allow-origin"), "*");
    assert.equal(resp.headers.get("access-control-allow-methods"), "GET,HEAD,POST,OPTIONS");
    assert.ok(resp.headers.get("access-control-allow-headers").includes("Authorization"));
    assert.ok(resp.headers.get("access-control-allow-headers").includes("Mcp-Session-Id"));
    assert.equal(resp.headers.get("access-control-expose-headers"), "WWW-Authenticate, Mcp-Session-Id");
    assert.equal(resp.headers.get("access-control-max-age"), "86400");
  }
});

test("every handled response carries CORS headers", async () => {
  const opts = makeOptions();
  const metadata = await GET("/.well-known/oauth-authorization-server", opts);
  assert.equal(metadata.headers.get("access-control-allow-origin"), "*");
  const reg = await POST("/oauth/register", { redirect_uris: ["https://x/cb"] }, opts);
  assert.equal(reg.headers.get("access-control-allow-origin"), "*");
  const token = await POST("/oauth/token", { grant_type: "x" }, opts);
  assert.equal(token.headers.get("access-control-allow-origin"), "*");
});

// ---------------------------------------------------------------------------
// 2. RFC 8414 metadata + aliases
// ---------------------------------------------------------------------------

test("all six authorization-server metadata paths serve the same document", async () => {
  const opts = makeOptions();
  const expected = oauthEdgeMetadata(IDENTITY);
  const paths = [
    "/.well-known/oauth-authorization-server",
    "/.well-known/openid-configuration",
    "/.well-known/oauth-authorization-server/mcp",
    "/.well-known/openid-configuration/mcp",
    "/mcp/.well-known/oauth-authorization-server",
    "/mcp/.well-known/openid-configuration",
  ];
  for (const path of paths) {
    const resp = await GET(path, opts);
    assert.equal(resp.status, 200);
    assert.deepEqual(await resp.json(), expected);
  }
});

test("metadata document is RFC8414 OAuth (no OIDC-only claims)", async () => {
  const opts = makeOptions();
  const doc = await (await GET("/.well-known/openid-configuration", opts)).json();
  assert.equal(doc.issuer, ISSUER);
  assert.equal(doc.token_endpoint, `${ISSUER}/oauth/token`);
  assert.deepEqual(doc.scopes_supported, ["mcp"]);
  assert.deepEqual(doc.token_endpoint_auth_methods_supported, ["none", "private_key_jwt", "client_secret_post"]);
  assert.equal(doc.authorization_response_iss_parameter_supported, true);
  assert.equal(doc.client_id_metadata_document_supported, true);
  assert.equal("userinfo_endpoint" in doc, false);
  assert.equal("id_token_signing_alg_values_supported" in doc, false);
});

// ---------------------------------------------------------------------------
// 3. RFC 9728 protected-resource metadata (root + /mcp aliases)
// ---------------------------------------------------------------------------

test("protected-resource metadata: root serves issuer; /mcp forms serve canonical", async () => {
  const opts = makeOptions();
  const root = await GET("/.well-known/oauth-protected-resource", opts);
  assert.deepEqual(await root.json(), protectedResourceEdgeMetadata(IDENTITY, IDENTITY.issuer));
  for (const path of ["/.well-known/oauth-protected-resource/mcp", "/mcp/.well-known/oauth-protected-resource"]) {
    const resp = await GET(path, opts);
    assert.deepEqual(await resp.json(), protectedResourceEdgeMetadata(IDENTITY, IDENTITY.resource));
  }
});

// ---------------------------------------------------------------------------
// 4. mcp.json
// ---------------------------------------------------------------------------

test("mcp.json serves server card with serverUrl=resource", async () => {
  const opts = makeOptions({ serverName: "herdr-mcp", serverVersion: "1.2.3" });
  const card = await (await GET("/.well-known/mcp.json", opts)).json();
  assert.deepEqual(card, { serverUrl: IDENTITY.resource, name: "herdr-mcp", version: "1.2.3" });
});

// ---------------------------------------------------------------------------
// 5. DCR aliases + persisted client lookup
// ---------------------------------------------------------------------------

test("all six DCR aliases register and the client persists via the DO", async () => {
  const paths = ["/oauth/register", "/register", "/register/", "/mcp/register", "/mcp/register/", "/mcp/oauth/register"];
  for (const path of paths) {
    const opts = makeOptions();
    const resp = await POST(path, { redirect_uris: ["https://app.example/cb"] }, opts);
    assert.equal(resp.status, 201, `alias ${path}`);
    assert.equal(resp.headers.get("cache-control"), "no-store");
    const data = await resp.json();
    assert.ok(data.client_id.startsWith("dcr-"));
    assert.ok(typeof data.client_secret === "string" && data.client_secret.length >= 32);
    assert.deepEqual(data.grant_types, ["authorization_code", "refresh_token"]);
    assert.equal(data.token_endpoint_auth_method, "none");
    assert.equal(data.client_secret_expires_at, 0);
    // Resolved from the DO (not synthesised).
    const found = await opts.store.getClient(data.client_id);
    assert.ok(found);
    assert.equal(found.client_secret_hash.length, 64, "store holds hex hash not raw secret");
    assert.notEqual(found.client_secret_hash, data.client_secret);
  }
});

test("DCR rejects missing redirect_uris; no client is created", async () => {
  const opts = makeOptions();
  const resp = await POST("/oauth/register", { scope: "mcp" }, opts);
  assert.equal(resp.status, 400);
  assert.equal((await resp.json()).error, "invalid_client_metadata");
  assert.equal(resp.headers.get("cache-control"), "no-store");
});

test("DCR client_secret_post stores SHA-256 hash, not the raw secret", async () => {
  const opts = makeOptions();
  const { client_id, client_secret } = await registerClient(opts, { token_endpoint_auth_method: "client_secret_post" });
  const stored = await opts.store.getClient(client_id);
  assert.equal(stored.token_endpoint_auth_method, "client_secret_post");
  assert.equal(stored.client_secret_hash, await sha256Hex(client_secret));
  assert.notEqual(stored.client_secret_hash, client_secret);
});

// ---------------------------------------------------------------------------
// 6. Authorization endpoint
// ---------------------------------------------------------------------------

test("authorize: first use requires fleet approval, then issues RFC9207 one-use code", async () => {
  const opts = makeOptions();
  const { client_id } = await registerClient(opts, { token_endpoint_auth_method: "none" });
  const pending = await pendingAuthorization(opts, client_id, "https://app.example/cb", "st123");
  const storedApproval = await opts.store.getApproval(pending.requestId, NOW_MS);
  assert.ok(storedApproval);
  assert.equal(
    storedApproval.approval_code_hash,
    await hashOAuthApprovalCode(APPROVAL_SECRET, pending.requestId, pending.approvalCode),
  );
  assert.notEqual(
    storedApproval.approval_code_hash,
    await hashOpaqueToken(`${pending.requestId}:${pending.approvalCode}`),
    "a DO storage snapshot must not expose an offline-verifiable six-digit code hash",
  );
  const approved = await approvePending(opts, pending);
  const loc = approved.location;
  assert.equal(loc.origin + loc.pathname, "https://app.example/cb");
  assert.ok(loc.searchParams.get("code"));
  assert.equal(loc.searchParams.get("state"), "st123");
  assert.equal(loc.searchParams.get("iss"), ISSUER); // RFC 9207

  // Exchange with the correct verifier (no secret needed — auth_method none).
  const tok = await POST("/oauth/token", tokenBody(client_id, loc.searchParams.get("code"), pending.verifier), opts);
  assert.equal(tok.status, 200);
  const pair = await tok.json();
  assert.equal(pair.token_type, "Bearer");
  assert.ok(pair.access_token.includes("."));
  assert.ok(pair.refresh_token);
  assert.equal(pair.scope, "mcp");
  assert.equal("key_id" in pair, false, "DO-only key_id must be stripped");

  // Issued access JWT verifies with the DO signing key.
  const verifierDo = await doVerifier(opts);
  const verdict = await verifierDo.verify(pair.access_token, NOW_SEC + 10);
  assert.equal(verdict.ok, true);
  assert.equal(verdict.clientId, client_id);

  // One-use: replay identical exchange fails.
  const replay = await POST("/oauth/token", tokenBody(client_id, loc.searchParams.get("code"), pending.verifier), opts);
  assert.equal(replay.status, 400);
  assert.equal((await replay.json()).error, "invalid_grant");
});

test("authorize: unknown client returns 400 JSON, no redirect", async () => {
  const opts = makeOptions();
  const qs = new URLSearchParams({ client_id: "unknown", redirect_uri: "https://app.example/cb", code_challenge: "a".repeat(43) });
  const resp = await GET(`/oauth/authorize?${qs}`, opts);
  assert.equal(resp.status, 400);
  assert.equal((await resp.json()).error, "invalid_request");
  assert.equal(resp.headers.get("location"), null);
});

test("authorize: unregistered redirect_uri is rejected (no redirect)", async () => {
  const opts = makeOptions();
  const { client_id } = await registerClient(opts);
  const qs = new URLSearchParams({ client_id, redirect_uri: "https://evil.example.com/cb", code_challenge: "a".repeat(43) });
  const resp = await GET(`/oauth/authorize?${qs}`, opts);
  assert.equal(resp.status, 400);
  assert.equal((await resp.json()).error_description, "redirect_uri is not registered for this client");
});

test("authorize: unsupported response_type / scope / resource → error redirects with iss", async () => {
  const opts = makeOptions();
  const { client_id } = await registerClient(opts);
  const challenge = await s256Challenge("B".repeat(44));
  const base = `client_id=${encodeURIComponent(client_id)}&redirect_uri=${encodeURIComponent("https://app.example/cb")}&state=s&code_challenge=${challenge}&code_challenge_method=S256`;

  const badType = await GET(`/oauth/authorize?${base}&response_type=token`, opts);
  assert.equal(badType.status, 302);
  assert.equal(new URL(badType.headers.get("location")).searchParams.get("error"), "unsupported_response_type");

  const badScope = await GET(`/oauth/authorize?${base}&scope=admin`, opts);
  assert.equal(new URL(badScope.headers.get("location")).searchParams.get("error"), "invalid_scope");

  const badResource = await GET(`/oauth/authorize?${base}&resource=${encodeURIComponent("https://evil.example.com/x")}`, opts);
  assert.equal(new URL(badResource.headers.get("location")).searchParams.get("error"), "invalid_target");

  for (const resp of [badType, badScope, badResource]) {
    assert.equal(new URL(resp.headers.get("location")).searchParams.get("iss"), ISSUER); // RFC 9207
  }
});

test("authorize: PKCE code_challenge required and S256 only", async () => {
  const opts = makeOptions();
  const { client_id } = await registerClient(opts);
  const base = `client_id=${encodeURIComponent(client_id)}&redirect_uri=${encodeURIComponent("https://app.example/cb")}`;
  const noChallenge = await GET(`/oauth/authorize?${base}`, opts);
  assert.equal(new URL(noChallenge.headers.get("location")).searchParams.get("error_description"), "PKCE code_challenge is required (S256)");
  const plain = await GET(`/oauth/authorize?${base}&code_challenge=${encodeURIComponent("a".repeat(43))}&code_challenge_method=plain`, opts);
  assert.equal(new URL(plain.headers.get("location")).searchParams.get("error_description"), "only code_challenge_method=S256 is supported");
});

// ---------------------------------------------------------------------------
// 7. CIMD authorization
// ---------------------------------------------------------------------------

test("authorize: ChatGPT CIMD client_id accepted with allowlisted redirect", async () => {
  const opts = makeOptions();
  const cimd = "https://chatgpt.com/oauth/connector/client/xyz";
  const redirect = "https://chatgpt.com/connector_platform_oauth_redirect";
  const { code, verifier } = await makeAuthCode(opts, cimd, redirect);
  assert.ok(code);
  // CIMD client can then exchange the code (no secret needed).
  const resp = await POST("/oauth/token", {
    grant_type: "authorization_code", code,
    redirect_uri: redirect, client_id: cimd, code_verifier: verifier, resource: IDENTITY.resource,
  }, opts);
  assert.equal(resp.status, 200);
  assert.ok((await resp.json()).access_token);
});

test("authorize: CIMD redirect NOT on allowlist is rejected", async () => {
  const opts = makeOptions();
  const cimd = "https://chatgpt.com/oauth/connector/client/xyz";
  const challenge = await s256Challenge("D".repeat(44));
  const qs = new URLSearchParams({ client_id: cimd, redirect_uri: "https://evil.com/cb", code_challenge: challenge });
  const resp = await GET(`/oauth/authorize?${qs}`, opts);
  assert.equal(resp.status, 400);
  assert.equal((await resp.json()).error_description, "redirect_uri is not registered for this client");
});

test("authorize+token: Claude URL client_id resolves its CIMD metadata without DCR", async () => {
  const cimd = "https://claude.ai/oauth/mcp-oauth-client-metadata";
  const redirect = "https://claude.ai/api/mcp/auth_callback";
  let metadataFetches = 0;
  const fetchFn = async (input, init = {}) => {
    assert.equal(String(input), cimd);
    assert.equal(init.method, "GET");
    assert.equal(init.redirect, "manual");
    metadataFetches += 1;
    return new Response(JSON.stringify({
      client_id: cimd,
      client_name: "Claude",
      redirect_uris: [redirect],
      grant_types: ["authorization_code", "refresh_token"],
      response_types: ["code"],
      token_endpoint_auth_method: "none",
    }), {
      status: 200,
      headers: { "content-type": "application/json" },
    });
  };
  const opts = makeOptions({ fetchFn });

  const { code, verifier } = await makeAuthCode(opts, cimd, redirect);
  assert.ok(code);
  const token = await POST("/oauth/token", {
    grant_type: "authorization_code",
    code,
    redirect_uri: redirect,
    client_id: cimd,
    code_verifier: verifier,
    resource: IDENTITY.resource,
  }, opts);
  assert.equal(token.status, 200);
  assert.ok((await token.json()).access_token);
  assert.equal(metadataFetches, 2, "authorize and token each validate current CIMD metadata");
});

test("authorize: generic CIMD keeps redirects manual and rejects 3xx metadata", async () => {
  const cimd = "https://client.example/oauth/client-metadata";
  const redirect = "https://client.example/oauth/callback";
  const challenge = await s256Challenge("R".repeat(44));
  const qs = new URLSearchParams({ client_id: cimd, redirect_uri: redirect, code_challenge: challenge });
  const opts = makeOptions({
    fetchFn: async (input, init = {}) => {
      assert.equal(String(input), cimd);
      assert.equal(init.redirect, "manual");
      return new Response(null, {
        status: 302,
        headers: { location: "https://redirect.example/oauth/client-metadata" },
      });
    },
  });

  const resp = await GET(`/oauth/authorize?${qs}`, opts);
  assert.equal(resp.status, 400);
  assert.equal((await resp.json()).error_description, "unknown client_id");
});

test("authorize: generic CIMD requires exact client_id and forbids shared-secret auth", async () => {
  const cimd = "https://client.example/oauth/client-metadata";
  const redirect = "https://client.example/oauth/callback";
  const challenge = await s256Challenge("G".repeat(44));
  const qs = new URLSearchParams({ client_id: cimd, redirect_uri: redirect, code_challenge: challenge });

  const mismatched = makeOptions({
    fetchFn: async () => new Response(JSON.stringify({
      client_id: `${cimd}/other`,
      redirect_uris: [redirect],
      token_endpoint_auth_method: "none",
    }), { status: 200, headers: { "content-type": "application/json" } }),
  });
  const mismatchResp = await GET(`/oauth/authorize?${qs}`, mismatched);
  assert.equal(mismatchResp.status, 400);
  assert.equal((await mismatchResp.json()).error_description, "unknown client_id");

  const sharedSecret = makeOptions({
    fetchFn: async () => new Response(JSON.stringify({
      client_id: cimd,
      redirect_uris: [redirect],
      token_endpoint_auth_method: "client_secret_post",
    }), { status: 200, headers: { "content-type": "application/json" } }),
  });
  const secretResp = await GET(`/oauth/authorize?${qs}`, sharedSecret);
  assert.equal(secretResp.status, 400);
  assert.equal((await secretResp.json()).error_description, "unknown client_id");
});

test("authorize: unsafe CIMD hosts are rejected before fetch", async () => {
  let fetched = false;
  const opts = makeOptions({ fetchFn: async () => { fetched = true; throw new Error("must not fetch"); } });
  const challenge = await s256Challenge("H".repeat(44));
  const qs = new URLSearchParams({
    client_id: "https://127.0.0.1/oauth/client-metadata",
    redirect_uri: "https://127.0.0.1/callback",
    code_challenge: challenge,
  });
  const resp = await GET(`/oauth/authorize?${qs}`, opts);
  assert.equal(resp.status, 400);
  assert.equal((await resp.json()).error_description, "unknown client_id");
  assert.equal(fetched, false);
});

// ---------------------------------------------------------------------------
// 8. Token endpoint: authorization_code + client auth
// ---------------------------------------------------------------------------

test("token: authorization_code with client_secret_post succeeds (no-store)", async () => {
  const opts = makeOptions();
  const { client_id, client_secret } = await registerClient(opts);
  const { code, verifier } = await makeAuthCode(opts, client_id);
  const resp = await POST("/oauth/token", tokenBody(client_id, code, verifier, { client_secret }), opts);
  assert.equal(resp.status, 200);
  assert.equal(resp.headers.get("cache-control"), "no-store");
  assert.ok((await resp.json()).access_token.includes("."));
});

test("authorization_code failures: client / redirect / resource / PKCE / secret", async () => {
  const opts = makeOptions();
  // Use auth_method none so the code ownership/redirect/resource/PKCE checks
  // are actually reached (client auth already covered elsewhere).
  const { client_id } = await registerClient(opts, { token_endpoint_auth_method: "none" });

  // wrong client: unknown client_id → invalid_client (auth check fires before code check)
  const { code: c0, verifier: v0 } = await makeAuthCode(opts, client_id);
  const badClient = await POST("/oauth/token", tokenBody("no-such-client", c0, v0), opts);
  assert.equal((await badClient.json()).error, "invalid_client");

  // wrong redirect
  const { code: c1, verifier: v1 } = await makeAuthCode(opts, client_id);
  const badRedirect = await POST("/oauth/token", tokenBody(client_id, c1, v1, { redirect_uri: "https://app.example/other" }), opts);
  assert.equal((await badRedirect.json()).error, "invalid_grant");

  // wrong resource (foreign — not just trailing-slash variant)
  const { code: c2, verifier: v2 } = await makeAuthCode(opts, client_id);
  const badResource = await POST("/oauth/token", tokenBody(client_id, c2, v2, { resource: "https://evil.example.com/x" }), opts);
  assert.equal((await badResource.json()).error, "invalid_target");

  // wrong PKCE
  const { code: c3, verifier: v3 } = await makeAuthCode(opts, client_id);
  const badPkce = await POST("/oauth/token", tokenBody(client_id, c3, v3, { code_verifier: "x".repeat(44) }), opts);
  assert.equal((await badPkce.json()).error, "invalid_grant");

  // wrong secret: needs a client_secret_post client so the secret is actually checked
  const { client_id: sc, client_secret: sc_secret } = await registerClient(opts, { token_endpoint_auth_method: "client_secret_post" });
  const { code: c5, verifier: v5 } = await makeAuthCode(opts, sc);
  const badSecret = await POST("/oauth/token", tokenBody(sc, c5, v5, { client_secret: "wrong" }), opts);
  assert.equal((await badSecret.json()).error, "invalid_client");
  // And the CORRECT secret for that client succeeds.
  const goodSecret = await POST("/oauth/token", tokenBody(sc, c5, v5, { client_secret: sc_secret }), opts);
  assert.equal(goodSecret.status, 200);
});

test("authorization_code: code issued to client A rejected when exchanged by known client B", async () => {
  const opts = makeOptions();
  const { client_id: cA } = await registerClient(opts, { token_endpoint_auth_method: "none" });
  const { client_id: cB } = await registerClient(opts, { token_endpoint_auth_method: "none" });
  const { code, verifier } = await makeAuthCode(opts, cA);

  const asB = await POST("/oauth/token", tokenBody(cB, code, verifier), opts);
  assert.equal(asB.status, 400);
  const asBData = await asB.json();
  assert.equal(asBData.error, "invalid_grant");
  assert.equal(asBData.error_description, "code was issued to a different client");

  // The code was consumed one-use by the failed exchange — replay by the real
  // owner must also fail.
  const ownerRetry = await POST("/oauth/token", tokenBody(cA, code, verifier), opts);
  assert.equal(ownerRetry.status, 400);
  assert.equal((await ownerRetry.json()).error, "invalid_grant");
});

test("token: HTTP Basic fallback authenticates when form fields absent", async () => {
  const opts = makeOptions();
  const { client_id, client_secret } = await registerClient(opts);
  const { code, verifier } = await makeAuthCode(opts, client_id);
  const basic = btoa(`${client_id}:${client_secret}`);
  const params = new URLSearchParams({
    grant_type: "authorization_code", code,
    redirect_uri: "https://app.example/cb", code_verifier: verifier, resource: IDENTITY.resource,
  });
  const resp = await POST("/oauth/token", params, opts, { form: true, headers: { authorization: `Basic ${basic}` } });
  assert.equal(resp.status, 200);
  assert.ok((await resp.json()).access_token);
});

test("token: auth_method none requires no secret", async () => {
  const opts = makeOptions();
  const { client_id } = await registerClient(opts, { token_endpoint_auth_method: "none" });
  const { code, verifier } = await makeAuthCode(opts, client_id);
  const resp = await POST("/oauth/token", tokenBody(client_id, code, verifier), opts);
  assert.equal(resp.status, 200);
});

test("token: client_credentials is reserved for approved automation clients and returns short-lived access only", async () => {
  const opts = makeOptions();
  const client_id = "svc_gitlab_pipeline_01";
  const client_secret = "herdr_svc_test-secret";
  const create = await opts.__do.fetch(new Request("https://oauth.internal/internal/oauth/automation/create", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      client_id,
      client_secret_hash: await sha256Hex(client_secret),
      client_name: "gitlab:group/project:prod",
      resource: IDENTITY.resource,
      scope: "mcp",
      created_by: "device:admin",
      now_ms: NOW_MS,
    }),
  }));
  assert.equal(create.status, 200);

  const wrongSecret = await POST("/oauth/token", {
    grant_type: "client_credentials",
    client_id,
    client_secret: "wrong",
    resource: IDENTITY.resource,
  }, opts);
  assert.equal(wrongSecret.status, 400);
  assert.equal((await wrongSecret.json()).error, "invalid_client");

  const response = await POST("/oauth/token", {
    grant_type: "client_credentials",
    client_id,
    client_secret,
    resource: IDENTITY.resource,
    scope: "mcp",
  }, opts);
  assert.equal(response.status, 200);
  assert.equal(response.headers.get("cache-control"), "no-store");
  const token = await response.json();
  assert.equal(token.expires_in, 3600);
  assert.equal(token.scope, "mcp");
  assert.equal(token.refresh_token, undefined);
  assert.ok(token.access_token.includes("."));

  const verify = await opts.__do.fetch(new Request("https://oauth.internal/internal/oauth/access/verify", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ token: token.access_token, now_sec: NOW_SEC + 1 }),
  }));
  assert.equal(verify.status, 200);

  assert.equal(await opts.store.revokeGrant(client_id, "device:admin", NOW_MS + 2000), true);
  const afterRevoke = await POST("/oauth/token", {
    grant_type: "client_credentials",
    client_id,
    client_secret,
    resource: IDENTITY.resource,
  }, opts);
  assert.equal(afterRevoke.status, 400);
  assert.equal((await afterRevoke.json()).error, "invalid_grant");
  const verifyRevoked = await opts.__do.fetch(new Request("https://oauth.internal/internal/oauth/access/verify", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ token: token.access_token, now_sec: NOW_SEC + 2 }),
  }));
  assert.equal(verifyRevoked.status, 401);
});

test("DCR cannot self-register a client_credentials automation principal", async () => {
  const opts = makeOptions();
  const registered = await registerClient(opts, {
    grant_types: ["client_credentials"],
    token_endpoint_auth_method: "client_secret_post",
  });
  const response = await POST("/oauth/token", {
    grant_type: "client_credentials",
    client_id: registered.client_id,
    client_secret: registered.client_secret,
    resource: IDENTITY.resource,
  }, opts);
  assert.equal(response.status, 400);
  assert.equal((await response.json()).error, "unauthorized_client");
});

test("token: unknown client_id is invalid_client", async () => {
  const opts = makeOptions();
  const resp = await POST("/oauth/token", {
    grant_type: "authorization_code", code: "x", client_id: "no-such-client",
    code_verifier: "a".repeat(44), resource: IDENTITY.resource,
  }, opts);
  assert.equal((await resp.json()).error, "invalid_client");
});

test("token: unsupported_grant_type", async () => {
  const opts = makeOptions();
  const { client_id } = await registerClient(opts, { token_endpoint_auth_method: "none" });
  const resp = await POST("/oauth/token", { grant_type: "implicit", client_id }, opts);
  assert.equal((await resp.json()).error, "unsupported_grant_type");
});

test("token: invalid foreign resource on any grant → invalid_target", async () => {
  const opts = makeOptions();
  const resp = await POST("/oauth/token", { grant_type: "refresh_token", resource: "https://evil.example.com/x" }, opts);
  assert.equal(resp.status, 400);
  assert.equal((await resp.json()).error, "invalid_target");
});

// ---------------------------------------------------------------------------
// 9. Refresh token rotation
// ---------------------------------------------------------------------------

test("refresh_token rotates: new pair issued, old token rejected on replay", async () => {
  const opts = makeOptions();
  const { client_id } = await registerClient(opts, { token_endpoint_auth_method: "none" });
  const { code, verifier } = await makeAuthCode(opts, client_id);
  const first = await POST("/oauth/token", tokenBody(client_id, code, verifier), opts);
  const { refresh_token, access_token } = await first.json();

  const rotated = await POST("/oauth/token", { grant_type: "refresh_token", refresh_token, client_id, resource: IDENTITY.resource }, opts);
  assert.equal(rotated.status, 200);
  assert.equal(rotated.headers.get("cache-control"), "no-store");
  const rot = await rotated.json();
  assert.notEqual(rot.refresh_token, refresh_token);
  assert.notEqual(rot.access_token, access_token);

  // Old refresh token is consumed → replay fails.
  const replay = await POST("/oauth/token", { grant_type: "refresh_token", refresh_token, client_id, resource: IDENTITY.resource }, opts);
  assert.equal(replay.status, 400);
  assert.equal((await replay.json()).error, "invalid_grant");

  // New refresh token still works.
  const again = await POST("/oauth/token", { grant_type: "refresh_token", refresh_token: rot.refresh_token, client_id, resource: IDENTITY.resource }, opts);
  assert.equal(again.status, 200);
});

test("refresh_token: missing token is invalid_grant", async () => {
  const opts = makeOptions();
  const { client_id } = await registerClient(opts, { token_endpoint_auth_method: "none" });
  const resp = await POST("/oauth/token", { grant_type: "refresh_token", client_id, resource: IDENTITY.resource }, opts);
  assert.equal((await resp.json()).error, "invalid_grant");
});

test("refresh_token: wrong client for an active refresh is rejected", async () => {
  const opts = makeOptions();
  const { client_id: cA } = await registerClient(opts, { token_endpoint_auth_method: "none" });
  const { client_id: cB } = await registerClient(opts, { token_endpoint_auth_method: "none" });
  const { code, verifier } = await makeAuthCode(opts, cA);
  const first = await POST("/oauth/token", tokenBody(cA, code, verifier), opts);
  const { refresh_token } = await first.json();
  const resp = await POST("/oauth/token", { grant_type: "refresh_token", refresh_token, client_id: cB, resource: IDENTITY.resource }, opts);
  assert.equal(resp.status, 400);
  assert.equal((await resp.json()).error, "invalid_grant");
});

// ---------------------------------------------------------------------------
// 10. ChatGPT CIMD private_key_jwt (real RS256, injected JWKS fetch)
// ---------------------------------------------------------------------------

async function generateJwk(kid) {
  const pair = await crypto.subtle.generateKey(
    { name: "RSASSA-PKCS1-v1_5", modulusLength: 2048, publicExponent: new Uint8Array([1, 0, 1]), hash: "SHA-256" },
    true, ["sign", "verify"],
  );
  const jwk = await crypto.subtle.exportKey("jwk", pair.publicKey);
  return { jwk: { ...jwk, alg: "RS256", use: "sig", kid }, privateKey: pair.privateKey };
}

async function signAssertion(payload, privateKey, kid) {
  const h = b64url(enc.encode(JSON.stringify({ alg: "RS256", typ: "JWT", kid })));
  const p = b64url(enc.encode(JSON.stringify(payload)));
  const data = enc.encode(`${h}.${p}`);
  const sig = new Uint8Array(await crypto.subtle.sign("RSASSA-PKCS1-v1_5", privateKey, data));
  return `${h}.${p}.${b64url(sig)}`;
}

function cimdAssertionBody(cimd, assertion, extra = {}) {
  const base = {
    grant_type: "authorization_code", code: "x",
    redirect_uri: "https://chatgpt.com/connector_platform_oauth_redirect",
    client_id: cimd, code_verifier: "a".repeat(44), resource: IDENTITY.resource,
    client_assertion: assertion,
    client_assertion_type: "urn:ietf:params:oauth:client-assertion-type:jwt-bearer",
  };
  return { ...base, ...extra };
}

test("token: ChatGPT CIMD private_key_jwt verified via injected fetch; no global network", async () => {
  const kid = "cimd-key-1";
  const { jwk, privateKey } = await generateJwk(kid);
  let fetched = 0;
  const fetchFn = async (url) => {
    fetched++;
    assert.ok(String(url).endsWith("/oauth/jwks.json"), `jwks url: ${url}`);
    return new Response(JSON.stringify({ keys: [jwk] }), { status: 200, headers: { "content-type": "application/json" } });
  };
  const cimd = "https://chatgpt.com/oauth/connector/client/cimd-1";
  const assertion = await signAssertion(
    { iss: cimd, aud: `${ISSUER}/oauth/token`, sub: cimd, iat: NOW_SEC, exp: NOW_SEC + 300 },
    privateKey, kid,
  );
  const opts = makeOptions({ fetchFn });
  // CIMD client_id is synthesized (no DCR needed), gets a real auth code.
  const { code, verifier } = await makeAuthCode(opts, cimd, "https://chatgpt.com/connector_platform_oauth_redirect");
  const resp = await POST("/oauth/token", cimdAssertionBody(cimd, assertion, { code, code_verifier: verifier }), opts);
  assert.equal(resp.status, 200);
  assert.ok((await resp.json()).access_token);
  assert.equal(fetched, 1, "JWKS fetched exactly once via injected fetchFn");
});

test("token: CIMD assertion failure maps to invalid_client", async () => {
  const { jwk, privateKey } = await generateJwk("key-bad");
  const fetchFn = async () => new Response(JSON.stringify({ keys: [jwk] }), { status: 200, headers: { "content-type": "application/json" } });
  const cimd = "https://chatgpt.com/oauth/connector/client/cimd-2";
  const bad = await signAssertion(
    { iss: "https://evil.example.com", aud: `${ISSUER}/oauth/token`, sub: cimd, iat: NOW_SEC, exp: NOW_SEC + 300 },
    privateKey, "key-bad",
  );
  const opts = makeOptions({ fetchFn });
  const resp = await POST("/oauth/token", cimdAssertionBody(cimd, bad), opts);
  assert.equal((await resp.json()).error, "invalid_client");
});

test("token: unsupported client_assertion_type is invalid_client", async () => {
  const opts = makeOptions();
  const cimd = "https://chatgpt.com/oauth/connector/client/cimd-type";
  const resp = await POST("/oauth/token", cimdAssertionBody(cimd, "abc", { client_assertion_type: "urn:other" }), opts);
  const data = await resp.json();
  assert.equal(data.error, "invalid_client");
  assert.equal(data.error_description, "unsupported client_assertion_type");
});

test("token: non-CIMD client with client_assertion is rejected", async () => {
  const opts = makeOptions();
  const { client_id } = await registerClient(opts);
  const resp = await POST("/oauth/token", {
    grant_type: "authorization_code", code: "x",
    redirect_uri: "https://app.example/cb", client_id, code_verifier: "a".repeat(44),
    resource: IDENTITY.resource, client_assertion: "abc", client_assertion_type: "urn:ietf:params:oauth:client-assertion-type:jwt-bearer",
  }, opts);
  assert.equal((await resp.json()).error, "invalid_client");
});

// ---------------------------------------------------------------------------
// 11. Body / query bounds + malformed JSON + no-store
// ---------------------------------------------------------------------------

test("oversized body → 413 invalid_request, no client created", async () => {
  const opts = makeOptions({ maxBodyBytes: 256 });
  const big = JSON.stringify({ redirect_uris: ["https://x/cb"], pad: "z".repeat(1000) });
  const resp = await POST("/oauth/register", big, opts, { form: false });
  assert.equal(resp.status, 413);
  const data = await resp.json();
  assert.equal(data.error, "invalid_request");
  assert.equal(data.error_description, "request body too large");
});

test("oversized JSON content-length → 413 invalid_request", async () => {
  const opts = makeOptions({ maxBodyBytes: 256 });
  const resp = await handleOAuthPublic(new Request("https://x.example/oauth/register", {
    method: "POST",
    headers: { "content-type": "application/json", "content-length": "999999" },
    body: JSON.stringify({ redirect_uris: ["https://x/cb"] }),
  }), opts);
  assert.equal(resp.status, 413);
  assert.equal((await resp.json()).error, "invalid_request");
});

test("oversized token body → 413 invalid_request", async () => {
  const opts = makeOptions({ maxBodyBytes: 256 });
  const resp = await handleOAuthPublic(new Request("https://x.example/oauth/token", {
    method: "POST",
    headers: { "content-type": "application/x-www-form-urlencoded" },
    body: "grant_type=refresh_token&pad=" + "z".repeat(1000),
  }), opts);
  assert.equal(resp.status, 413);
  assert.equal((await resp.json()).error, "invalid_request");
});

test("malformed JSON body → 400 invalid_request", async () => {
  const opts = makeOptions();
  const resp = await handleOAuthPublic(new Request("https://x.example/oauth/register", {
    method: "POST", headers: { "content-type": "application/json" }, body: "{not json",
  }), opts);
  assert.equal(resp.status, 400);
  assert.equal((await resp.json()).error, "invalid_request");
});

test("oversized authorization query → 400 invalid_request", async () => {
  const opts = makeOptions({ maxQueryBytes: 128 });
  const url = `/oauth/authorize?${"a".repeat(200)}`;
  const resp = await GET(url, opts);
  assert.equal(resp.status, 400);
  assert.equal((await resp.json()).error, "invalid_request");
});

// ---------------------------------------------------------------------------
// 12. Unmatched routes return null (index.ts fallthrough)
// ---------------------------------------------------------------------------

test("non-owned routes return null", async () => {
  const opts = makeOptions();
  for (const [method, path] of [
    ["GET", "/mcp"],
    ["POST", "/mcp"],
    ["GET", "/health"],
    ["GET", "/.well-known/unknown"],
    ["GET", "/oauth/authorize-mistake"],
    ["DELETE", "/oauth/register"],
    ["GET", "/oauth/register"],
  ]) {
    const resp = await handleOAuthPublic(new Request(`https://x.example${path}`, { method }), opts);
    assert.equal(resp, null, `expected null for ${method} ${path}`);
  }
});