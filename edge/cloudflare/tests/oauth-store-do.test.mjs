import { test } from "node:test";
import assert from "node:assert/strict";
import { OAuthStoreDO, normalizeOAuthClient, normalizeOAuthCode, normalizeOAuthToken } from "../dist/oauth-store-do.js";
import { createOAuthIdentity, createRs256AccessTokenVerifier, hashOpaqueToken } from "../dist/oauth-edge.js";

// ---- test helpers for production-like JWK key pairs ----

async function generateJwkPair(kid) {
  const pair = await crypto.subtle.generateKey(
    { name: "RSASSA-PKCS1-v1_5", modulusLength: 2048, publicExponent: new Uint8Array([1, 0, 1]), hash: "SHA-256" },
    true,
    ["sign", "verify"],
  );
  const priv = await crypto.subtle.exportKey("jwk", pair.privateKey);
  const pub = await crypto.subtle.exportKey("jwk", pair.publicKey);
  const decorate = (jwk, kid) => ({ ...jwk, kid, alg: "RS256", use: "sig" });
  return {
    private_jwk: decorate(priv, kid),
    public_jwk: decorate(pub, kid),
    kid,
  };
}

function signingPayload(pair, created = 100) {
  return {
    kid: pair.kid,
    private_jwk: pair.private_jwk,
    public_jwk: pair.public_jwk,
    created_at: created,
  };
}

async function importPublicJwk(pair) {
  return crypto.subtle.importKey(
    "jwk",
    pair.public_jwk,
    { name: "RSASSA-PKCS1-v1_5", hash: "SHA-256" },
    false,
    ["verify"],
  );
}

async function verifyAccessWithPublicKey(token, pubKey, nowSec) {
  const verifier = createRs256AccessTokenVerifier(createOAuthIdentity("https://issuer"), pubKey);
  return verifier.verify(token, nowSec);
}


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

function harness(env = { OAUTH_ISSUER: "https://issuer" }) {
  const storage = new StorageMock();
  const state = { storage };
  const instance = new OAuthStoreDO(state, env);
  const post = async (path, body) => instance.fetch(new Request(`https://do.internal${path}`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  }));
  const get = async (path) => instance.fetch(new Request(`https://do.internal${path}`));
  return { storage, instance, post, get };
}

const client = {
  client_secret_hash: "abc",
  redirect_uris: ["https://example.com/callback"],
  token_endpoint_auth_method: "client_secret_post",
  grant_types: ["authorization_code", "refresh_token"],
  scope: "mcp",
  issued_at: 100,
};
const token = (exp = 5000) => ({ client_id: "c1", resource: "https://issuer/mcp", scope: "mcp", expires_at: exp });
const code = (exp = 5000) => ({ client_id: "c1", redirect_uri: "https://example.com/cb", code_challenge: "x".repeat(43), resource: "https://issuer/mcp", expires_at: exp });
const approval = (overrides = {}) => ({
  client_id: "c1",
  redirect_uri: "https://example.com/cb",
  code_challenge: "x".repeat(43),
  resource: "https://issuer/mcp",
  scope: "mcp",
  state: "state-1",
  approval_code_hash: "approval-hash",
  resume_hash: "resume-hash",
  created_at_ms: 100,
  expires_at_ms: 10_000,
  attempts: 0,
  status: "pending",
  ...overrides,
});

async function body(response) { return response.json(); }

test("normalizers enforce bounded OAuth state", () => {
  assert.equal(normalizeOAuthClient(client).token_endpoint_auth_method, "client_secret_post");
  assert.equal(normalizeOAuthClient({ ...client, redirect_uris: "no" }), null);
  assert.equal(normalizeOAuthToken(token(200), 100).expires_at, 200);
  assert.equal(normalizeOAuthToken(token(100), 100), null);
  assert.equal(normalizeOAuthCode(code(200), 100).expires_at, 200);
  assert.equal(normalizeOAuthCode(code(100), 100), null);
});

test("client put/get and token put/get", async () => {
  const h = harness();
  assert.equal((await body(await h.post("/internal/oauth/client/put", { client_id: "c1", record: client }))).ok, true);
  const c = await body(await h.post("/internal/oauth/client/get", { client_id: "c1" }));
  assert.equal(c.ok, true);
  assert.equal(c.record.client_secret_hash, "abc");
  assert.equal((await body(await h.post("/internal/oauth/access/put", { hash: "h1", record: token(5000), now_sec: 100 }))).ok, true);
  const t = await body(await h.post("/internal/oauth/access/get", { hash: "h1", now_sec: 100 }));
  assert.equal(t.record.client_id, "c1");
});

test("refresh consume is atomic one-use and deletes before replay", async () => {
  const h = harness();
  await h.post("/internal/oauth/refresh/put", { hash: "rh1", record: token(5000), now_sec: 100 });
  const first = await body(await h.post("/internal/oauth/refresh/consume", { hash: "rh1", now_sec: 100 }));
  assert.equal(first.ok, true);
  assert.equal(first.record.client_id, "c1");
  const secondResp = await h.post("/internal/oauth/refresh/consume", { hash: "rh1", now_sec: 100 });
  assert.equal(secondResp.status, 404);
  assert.equal((await body(secondResp)).code, "not_found");
});

test("expired refresh is deleted and cannot replay", async () => {
  const h = harness();
  h.storage.map.set("refresh:rh-expired", token(99));
  const first = await h.post("/internal/oauth/refresh/consume", { hash: "rh-expired", now_sec: 100 });
  assert.equal(first.status, 404);
  assert.equal((await body(first)).code, "expired");
  const second = await h.post("/internal/oauth/refresh/consume", { hash: "rh-expired", now_sec: 100 });
  assert.equal((await body(second)).code, "not_found");
});

test("authorization code consume is one-use and expiry-aware", async () => {
  const h = harness();
  await h.post("/internal/oauth/code/put", { hash: "ch1", record: code(5000), now_ms: 100 });
  assert.equal((await body(await h.post("/internal/oauth/code/consume", { hash: "ch1", now_ms: 100 }))).ok, true);
  assert.equal((await h.post("/internal/oauth/code/consume", { hash: "ch1", now_ms: 100 })).status, 404);
  h.storage.map.set("code:expired", code(99));
  const expired = await h.post("/internal/oauth/code/consume", { hash: "expired", now_ms: 100 });
  assert.equal((await body(expired)).code, "expired");
  assert.equal(h.storage.map.has("code:expired"), false);
});

test("connector approval is request-bound, five wrong attempts lock it, and correct approval creates a grant", async () => {
  const h = harness();
  assert.equal((await body(await h.post("/internal/oauth/approval/put", {
    request_id: "req-1",
    record: approval(),
    now_ms: 100,
  }))).ok, true);

  for (let attempt = 1; attempt <= 4; attempt++) {
    const wrong = await h.post("/internal/oauth/approval/approve", {
      request_id: "req-1",
      code_hash: `wrong-${attempt}`,
      approver: "device:owner",
      now_ms: 200 + attempt,
    });
    assert.equal(wrong.status, 403);
    assert.equal((await body(wrong)).code, "invalid_code");
  }
  const locked = await h.post("/internal/oauth/approval/approve", {
    request_id: "req-1",
    code_hash: "wrong-5",
    approver: "device:owner",
    now_ms: 300,
  });
  assert.equal(locked.status, 423);
  assert.equal((await body(locked)).code, "locked");
  const correctAfterLock = await h.post("/internal/oauth/approval/approve", {
    request_id: "req-1",
    code_hash: "approval-hash",
    approver: "device:owner",
    now_ms: 301,
  });
  assert.equal(correctAfterLock.status, 423);

  await h.post("/internal/oauth/approval/put", {
    request_id: "req-2",
    record: approval({ approval_code_hash: "good", resume_hash: "resume-2" }),
    now_ms: 100,
  });
  const ok = await body(await h.post("/internal/oauth/approval/approve", {
    request_id: "req-2",
    code_hash: "good",
    approver: "device:owner",
    now_ms: 400,
  }));
  assert.equal(ok.ok, true);
  assert.equal(ok.record.client_id, "c1");
  const grant = await body(await h.post("/internal/oauth/grant/get", { client_id: "c1" }));
  assert.equal(grant.record.status, "active");
  assert.equal(grant.record.can_approve_connectors, true);
  assert.equal(grant.record.approved_by, "device:owner");

  await h.post("/internal/oauth/approval/put", {
    request_id: "req-delegated",
    record: approval({ client_id: "c2", approval_code_hash: "delegated-good", resume_hash: "delegated-resume" }),
    now_ms: 401,
  });
  const delegated = await body(await h.post("/internal/oauth/approval/approve", {
    request_id: "req-delegated",
    code_hash: "delegated-good",
    approver: "oauth:c1",
    now_ms: 500,
  }));
  assert.equal(delegated.ok, true);
  const delegatedGrant = await body(await h.post("/internal/oauth/grant/get", { client_id: "c2" }));
  assert.equal(delegatedGrant.record.status, "active");
  assert.equal(delegatedGrant.record.can_approve_connectors, false);
  assert.equal(delegatedGrant.record.approved_by, "oauth:c1");
});

test("connector approval resume token is independent, one-use, and cannot be substituted", async () => {
  const h = harness();
  await h.post("/internal/oauth/approval/put", {
    request_id: "req-resume",
    record: approval({ approval_code_hash: "good", resume_hash: "resume-good" }),
    now_ms: 100,
  });
  await h.post("/internal/oauth/approval/approve", {
    request_id: "req-resume",
    code_hash: "good",
    approver: "device:owner",
    now_ms: 200,
  });
  const wrongResume = await h.post("/internal/oauth/approval/consume", {
    request_id: "req-resume",
    resume_hash: "resume-wrong",
    now_ms: 300,
  });
  assert.equal(wrongResume.status, 403);
  assert.equal((await body(wrongResume)).code, "invalid_resume");

  const first = await body(await h.post("/internal/oauth/approval/consume", {
    request_id: "req-resume",
    resume_hash: "resume-good",
    now_ms: 301,
  }));
  assert.equal(first.ok, true);
  assert.equal(first.record.redirect_uri, "https://example.com/cb");
  assert.equal(first.record.code_challenge, "x".repeat(43));
  assert.equal(first.record.state, "state-1");

  const replay = await h.post("/internal/oauth/approval/consume", {
    request_id: "req-resume",
    resume_hash: "resume-good",
    now_ms: 302,
  });
  assert.equal(replay.status, 404);
});

test("connector grant revoke fences issued JWT and refresh credentials while legacy no-grant access remains compatible", async () => {
  const h = harness();
  const legacyIssued = await body(await h.post("/internal/oauth/token/issue", {
    client_id: "legacy-client",
    resource: "https://issuer/mcp",
    now_sec: 1000,
    access_ttl_sec: 3600,
    refresh_ttl_sec: 10000,
  }));
  const legacyVerify = await h.post("/internal/oauth/access/verify", {
    token: legacyIssued.token.access_token,
    now_sec: 1001,
  });
  assert.equal(legacyVerify.status, 200, "pre-v0.4.6 clients without grant records keep ordinary access");

  await h.post("/internal/oauth/approval/put", {
    request_id: "req-revoke",
    record: approval({ client_id: "c-revoke", approval_code_hash: "good", resume_hash: "resume" }),
    now_ms: 100,
  });
  await h.post("/internal/oauth/approval/approve", {
    request_id: "req-revoke",
    code_hash: "good",
    approver: "device:owner",
    now_ms: 200,
  });
  const issued = await body(await h.post("/internal/oauth/token/issue", {
    client_id: "c-revoke",
    resource: "https://issuer/mcp",
    now_sec: 1000,
    access_ttl_sec: 3600,
    refresh_ttl_sec: 10000,
  }));
  assert.equal((await h.post("/internal/oauth/access/verify", { token: issued.token.access_token, now_sec: 1001 })).status, 200);

  const revoked = await body(await h.post("/internal/oauth/grant/revoke", {
    client_id: "c-revoke",
    revoked_by: "device:owner",
    now_ms: 400,
  }));
  assert.equal(revoked.ok, true);
  assert.equal((await h.post("/internal/oauth/access/verify", { token: issued.token.access_token, now_sec: 1002 })).status, 401);
  const refreshHash = await hashOpaqueToken(issued.token.refresh_token);
  assert.equal((await h.post("/internal/oauth/refresh/get", { hash: refreshHash, now_sec: 1002 })).status, 404);
  assert.equal((await h.post("/internal/oauth/token/issue", {
    client_id: "c-revoke",
    resource: "https://issuer/mcp",
    now_sec: 1002,
    access_ttl_sec: 3600,
    refresh_ttl_sec: 10000,
  })).status, 400);
});

test("signing key is generated once, private JWK never leaves the internal public response", async () => {
  const h = harness();
  const first = await body(await h.post("/internal/oauth/signing/ensure", {}));
  const second = await body(await h.post("/internal/oauth/signing/ensure", {}));
  assert.equal(first.ok, true);
  assert.equal(first.kid, second.kid);
  assert.equal(typeof first.public_jwk.n, "string");
  assert.equal(first.public_jwk.d, undefined);
  const stored = h.storage.map.get("signing:key:v1");
  assert.equal(typeof stored.private_jwk.d, "string");
});

test("token issue + refresh exchange uses Edge signing key and rotates one-time refresh", async () => {
  const h = harness();
  const issued = await body(await h.post("/internal/oauth/token/issue", {
    client_id: "c1",
    resource: "https://issuer/mcp",
    now_sec: 1000,
    access_ttl_sec: 3600,
    refresh_ttl_sec: 10000,
  }));
  assert.equal(issued.ok, true);
  assert.equal(issued.token.token_type, "Bearer");
  assert.equal(typeof issued.token.access_token, "string");
  assert.equal(typeof issued.token.refresh_token, "string");

  const signing = await body(await h.post("/internal/oauth/signing/ensure", {}));
  const publicKey = await crypto.subtle.importKey(
    "jwk",
    signing.public_jwk,
    { name: "RSASSA-PKCS1-v1_5", hash: "SHA-256" },
    false,
    ["verify"],
  );
  const verifier = createRs256AccessTokenVerifier(createOAuthIdentity("https://issuer"), publicKey);
  const verdict = await verifier.verify(issued.token.access_token, 1001);
  assert.equal(verdict.ok, true);
  assert.equal(verdict.clientId, "c1");

  const oldHash = await hashOpaqueToken(issued.token.refresh_token);
  const exchanged = await body(await h.post("/internal/oauth/refresh/exchange", {
    hash: oldHash,
    client_id: "c1",
    resource: "https://issuer/mcp",
    now_sec: 1100,
    access_ttl_sec: 3600,
    refresh_ttl_sec: 10000,
  }));
  assert.equal(exchanged.ok, true);
  assert.notEqual(exchanged.token.refresh_token, issued.token.refresh_token);

  const replay = await h.post("/internal/oauth/refresh/exchange", {
    hash: oldHash,
    client_id: "c1",
    resource: "https://issuer/mcp",
    now_sec: 1101,
    access_ttl_sec: 3600,
    refresh_ttl_sec: 10000,
  });
  assert.equal(replay.status, 400);
  assert.equal((await body(replay)).code, "invalid_grant");
});

test("bulk import is bounded, validates, and is idempotent without overwrite", async () => {
  const h = harness();
  const payload = {
    clients: { c1: client, bad: { redirect_uris: "bad" } },
    tokens: { t1: token(5000), expired: token(50) },
    refresh: { r1: token(6000) },
    now_sec: 100,
  };
  const first = await body(await h.post("/internal/oauth/import", payload));
  assert.equal(first.ok, true);
  assert.deepEqual(first.result.clients, { imported: 1, skipped: 0, invalid: 1 });
  assert.deepEqual(first.result.tokens, { imported: 1, skipped: 0, invalid: 1 });
  assert.deepEqual(first.result.refresh, { imported: 1, skipped: 0, invalid: 0 });
  const second = await body(await h.post("/internal/oauth/import", payload));
  assert.deepEqual(second.result.clients, { imported: 0, skipped: 1, invalid: 1 });
  assert.deepEqual(second.result.tokens, { imported: 0, skipped: 1, invalid: 1 });
  assert.deepEqual(second.result.refresh, { imported: 0, skipped: 1, invalid: 0 });
  const stats = await body(await h.get("/internal/oauth/stats"));
  assert.deepEqual(stats, { ok: true, clients: 1, access: 1, refresh: 1, codes: 0, approvals: 0, grants: 0 });
});

test("bulk import rejects too many records and large declared bodies", async () => {
  const h = harness();
  const many = Object.fromEntries(Array.from({ length: 513 }, (_, i) => [`k${i}`, token(5000)]));
  const tooMany = await h.post("/internal/oauth/import", { tokens: many, now_sec: 100 });
  assert.equal(tooMany.status, 413);
  assert.equal((await body(tooMany)).code, "too_many_records");

  const declared = await h.instance.fetch(new Request("https://do.internal/internal/oauth/import", {
    method: "POST",
    headers: { "content-type": "application/json", "content-length": String(300 * 1024) },
    body: "{}",
  }));
  assert.equal(declared.status, 413);
});

// ---------------------------------------------------------------------------
// Signing-key continuity: inject production RS256 pair via bounded import
// ---------------------------------------------------------------------------

test("signing key import: injected production pair signs issued tokens and is usable for refresh rotation", async () => {
  const h = harness();
  const pair = await generateJwkPair("prod-kid-1");
  const imp = await body(await h.post("/internal/oauth/import", {
    now_sec: 100,
    overwrite: true,
    signing_key: signingPayload(pair),
  }));
  assert.equal(imp.ok, true);
  assert.deepEqual(imp.result.signing_key, { imported: 1, skipped: 0 });

  // Issued access JWT must verify with the injected production public key.
  const issued = await body(await h.post("/internal/oauth/token/issue", {
    client_id: "c1",
    resource: "https://issuer/mcp",
    now_sec: 1000,
    access_ttl_sec: 3600,
    refresh_ttl_sec: 10000,
  }));
  const issuedVerdict = await verifyAccessWithPublicKey(issued.token.access_token, await importPublicJwk(pair), 1001);
  assert.equal(issuedVerdict.ok, true);
  assert.equal(issuedVerdict.clientId, "c1");

  // Refresh rotation issues a NEW access JWT with the SAME migrated key.
  const oldHash = await hashOpaqueToken(issued.token.refresh_token);
  const exchanged = await body(await h.post("/internal/oauth/refresh/exchange", {
    hash: oldHash,
    client_id: "c1",
    resource: "https://issuer/mcp",
    now_sec: 1100,
    access_ttl_sec: 3600,
    refresh_ttl_sec: 10000,
  }));
  assert.equal(exchanged.ok, true);
  const rotatedVerdict = await verifyAccessWithPublicKey(exchanged.token.access_token, await importPublicJwk(pair), 1101);
  assert.equal(rotatedVerdict.ok, true);
  assert.equal(rotatedVerdict.clientId, "c1");
});

test("signing/ensure never exposes private JWK fields after injection", async () => {
  const h = harness();
  const pair = await generateJwkPair("prod-kid-2");
  await h.post("/internal/oauth/import", { now_sec: 100, overwrite: true, signing_key: signingPayload(pair) });
  const resp = await body(await h.post("/internal/oauth/signing/ensure", {}));
  assert.equal(resp.ok, true);
  assert.equal(resp.kid, "prod-kid-2");
  assert.equal(typeof resp.public_jwk.n, "string");
  assert.equal(resp.public_jwk.d, undefined);
  assert.equal(resp.private_jwk, undefined);
  // Stored record keeps the private key (continuity) but the public response never returns it.
  const stored = h.storage.map.get("signing:key:v1");
  assert.equal(typeof stored.private_jwk.d, "string");
});

test("signing key overwrite/rollback: re-import restores the previous production key", async () => {
  const h = harness();
  const keyA = await generateJwkPair("rollback-key-a");
  const keyB = await generateJwkPair("rollback-key-b");
  await h.post("/internal/oauth/import", { now_sec: 100, overwrite: true, signing_key: signingPayload(keyA) });
  const pubA = await importPublicJwk(keyA);
  const pubB = await importPublicJwk(keyB);

  const issue = async () => body(await h.post("/internal/oauth/token/issue", {
    client_id: "c1", resource: "https://issuer/mcp", now_sec: 1000, access_ttl_sec: 3600, refresh_ttl_sec: 10000,
  }));

  // Key A active.
  assert.equal((await verifyAccessWithPublicKey((await issue()).token.access_token, pubA, 1001)).ok, true);

  // Switch to key B (overwrite).
  const bImp = await body(await h.post("/internal/oauth/import", { now_sec: 100, overwrite: true, signing_key: signingPayload(keyB) }));
  assert.deepEqual(bImp.result.signing_key, { imported: 1, skipped: 0 });
  assert.equal((await verifyAccessWithPublicKey((await issue()).token.access_token, pubB, 1001)).ok, true);

  // Rollback to key A (overwrite) — new JWTs follow the active key again.
  const aImp = await body(await h.post("/internal/oauth/import", { now_sec: 100, overwrite: true, signing_key: signingPayload(keyA) }));
  assert.deepEqual(aImp.result.signing_key, { imported: 1, skipped: 0 });
  assert.equal((await verifyAccessWithPublicKey((await issue()).token.access_token, pubA, 1001)).ok, true);
});

test("signing key import without overwrite preserves the active key", async () => {
  const h = harness();
  const keyA = await generateJwkPair("keep-key-a");
  const keyB = await generateJwkPair("keep-key-b");
  await h.post("/internal/oauth/import", { now_sec: 100, overwrite: true, signing_key: signingPayload(keyA) });
  const skip = await body(await h.post("/internal/oauth/import", { now_sec: 100, signing_key: signingPayload(keyB) }));
  assert.deepEqual(skip.result.signing_key, { imported: 0, skipped: 1 });
  const issued = await body(await h.post("/internal/oauth/token/issue", {
    client_id: "c1", resource: "https://issuer/mcp", now_sec: 1000, access_ttl_sec: 3600, refresh_ttl_sec: 10000,
  }));
  assert.equal((await verifyAccessWithPublicKey(issued.token.access_token, await importPublicJwk(keyA), 1001)).ok, true);
});

test("invalid signing key pair is rejected before any state mutation; prior key stays usable", async () => {
  const h = harness();
  const good = await generateJwkPair("good-key");
  await h.post("/internal/oauth/import", { now_sec: 100, overwrite: true, signing_key: signingPayload(good) });

  // Mismatched n/e (public from a different key) → invalid_signing_key, and
  // the import must NOT write the accompanying client record.
  const other = await generateJwkPair("other-key");
  const badPair = {
    kid: "bad-pair",
    private_jwk: good.private_jwk,
    public_jwk: other.public_jwk,
    created_at: 100,
  };
  const bad = await h.post("/internal/oauth/import", {
    now_sec: 100,
    overwrite: true,
    signing_key: badPair,
    clients: { c1: client },
  });
  assert.equal(bad.status, 400);
  assert.equal((await body(bad)).code, "invalid_signing_key");
  // Client not imported (validation happened before mutate).
  const absent = await h.post("/internal/oauth/client/get", { client_id: "c1" });
  assert.equal(absent.status, 404);

  // Prior key still fully usable.
  const issued = await body(await h.post("/internal/oauth/token/issue", {
    client_id: "c1", resource: "https://issuer/mcp", now_sec: 1000, access_ttl_sec: 3600, refresh_ttl_sec: 10000,
  }));
  assert.equal((await verifyAccessWithPublicKey(issued.token.access_token, await importPublicJwk(good), 1001)).ok, true);
});

test("signing key import: structurally invalid pairs are rejected generically", async () => {
  const h = harness();
  const pair = await generateJwkPair("struct-kid");
  const cases = [
    // public key carries private exponent d
    { ...signingPayload(pair), public_jwk: { ...pair.public_jwk, d: "AAAA" } },
    // missing private exponent on private key
    { ...signingPayload(pair), private_jwk: { ...pair.private_jwk, d: undefined } },
    // non-RSA kty
    { ...signingPayload(pair), public_jwk: { ...pair.public_jwk, kty: "EC" } },
    // wrong alg
    { ...signingPayload(pair), private_jwk: { ...pair.private_jwk, alg: "RS512" } },
    // not an object
    42,
  ];
  for (const signing_key of cases) {
    const resp = await h.post("/internal/oauth/import", { now_sec: 100, overwrite: true, signing_key });
    assert.equal(resp.status, 400, `expected 400 for ${JSON.stringify(signing_key).slice(0, 60)}`);
    assert.equal((await body(resp)).code, "invalid_signing_key");
  }
});

test("signing key errors never echo kid, private modulus, or private exponent", async () => {
  const h = harness();
  const pair = await generateJwkPair("secret-prod-kid");
  const other = await generateJwkPair("other-prod-kid");
  const bad = {
    kid: "secret-prod-kid",
    private_jwk: pair.private_jwk,
    public_jwk: other.public_jwk, // mismatch → invalid
    created_at: 100,
  };
  const resp = await h.post("/internal/oauth/import", { now_sec: 100, overwrite: true, signing_key: bad });
  const text = JSON.stringify(await body(resp));
  assert.equal(resp.status, 400);
  assert.ok(text.includes("invalid_signing_key"), text);
  // No key material leaks into the error body.
  assert.equal(text.includes("secret-prod-kid"), false);
  assert.equal(text.includes(pair.private_jwk.n), false);
  assert.equal(text.includes(pair.private_jwk.d), false);
  assert.equal(text.includes(pair.public_jwk.n), false);
});
