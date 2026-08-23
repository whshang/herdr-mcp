import { test } from "node:test";
import assert from "node:assert/strict";
import {
  createOAuthIdentity,
  normalizeResource,
  oauthEdgeMetadata,
  protectedResourceEdgeMetadata,
  mcpServerCardMetadata,
  base64urlDecode,
  hashOpaqueToken,
  createRs256AccessTokenVerifier,
  resolveAccessToken,
  normalizeStoredClient,
  normalizeStoredToken,
} from "../dist/oauth-edge.js";

// ---------------------------------------------------------------------------
// Test helpers: real RS256 keypair + compact-JWT signing via Web Crypto
// ---------------------------------------------------------------------------

const enc = new TextEncoder();

function base64urlEncode(bytes) {
  let s = "";
  for (const b of bytes) s += String.fromCharCode(b);
  return btoa(s).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

async function generateRsaKeypair() {
  return crypto.subtle.generateKey(
    {
      name: "RSASSA-PKCS1-v1_5",
      modulusLength: 2048,
      publicExponent: new Uint8Array([1, 0, 1]),
      hash: "SHA-256",
    },
    true,
    ["sign", "verify"],
  );
}

async function signJwt(payload, privateKey, header = { alg: "RS256", typ: "at+jwt" }) {
  const h = base64urlEncode(enc.encode(JSON.stringify(header)));
  const p = base64urlEncode(enc.encode(JSON.stringify(payload)));
  const data = enc.encode(`${h}.${p}`);
  const sig = await crypto.subtle.sign("RSASSA-PKCS1-v1_5", privateKey, data);
  return `${h}.${p}.${base64urlEncode(new Uint8Array(sig))}`;
}

// ---------------------------------------------------------------------------
// Metadata + issuer/resource exactness
// ---------------------------------------------------------------------------

test("createOAuthIdentity derives resource and strips trailing slashes", () => {
  const id = createOAuthIdentity("https://herdr-mcp.example.com/");
  assert.equal(id.issuer, "https://herdr-mcp.example.com");
  assert.equal(id.resource, "https://herdr-mcp.example.com/mcp");
  // No default issuer: fail closed rather than invent one.
  assert.throws(() => createOAuthIdentity(""), /issuer is required/);
  assert.throws(() => createOAuthIdentity("   "), /issuer is required/);
});

test("normalizeResource maps '' / issuer / issuer+/mcp and rejects foreign", () => {
  const id = createOAuthIdentity("https://herdr-mcp.example.com");
  assert.equal(normalizeResource(id, ""), id.resource);
  assert.equal(normalizeResource(id, "https://herdr-mcp.example.com"), id.resource);
  assert.equal(normalizeResource(id, "https://herdr-mcp.example.com/"), id.resource);
  assert.equal(normalizeResource(id, "https://herdr-mcp.example.com/mcp"), id.resource);
  assert.equal(normalizeResource(id, "https://herdr-mcp.example.com/mcp/"), id.resource);
  assert.equal(normalizeResource(id, "https://evil.example.com/mcp"), null);
  assert.equal(normalizeResource(id, "https://herdr-mcp.example.com/other"), null);
});

test("oauthEdgeMetadata deep-equals the src/oauth.ts document (keys + values + order)", () => {
  const id = createOAuthIdentity("https://herdr-mcp.example.com");
  const doc = oauthEdgeMetadata(id);
  assert.deepEqual(doc, {
    issuer: "https://herdr-mcp.example.com",
    authorization_endpoint: "https://herdr-mcp.example.com/oauth/authorize",
    token_endpoint: "https://herdr-mcp.example.com/oauth/token",
    registration_endpoint: "https://herdr-mcp.example.com/oauth/register",
    scopes_supported: ["mcp"],
    response_types_supported: ["code"],
    grant_types_supported: ["authorization_code", "refresh_token"],
    token_endpoint_auth_methods_supported: ["none", "private_key_jwt", "client_secret_post"],
    code_challenge_methods_supported: ["S256"],
    authorization_response_iss_parameter_supported: true,
    client_id_metadata_document_supported: true,
    protected_resources: ["https://herdr-mcp.example.com/mcp"],
  });
  // Exact key set — any drift from the local runtime's discovery doc is a cutover break.
  assert.deepEqual(Object.keys(doc).sort(), [
    "authorization_endpoint",
    "authorization_response_iss_parameter_supported",
    "client_id_metadata_document_supported",
    "code_challenge_methods_supported",
    "grant_types_supported",
    "issuer",
    "protected_resources",
    "registration_endpoint",
    "response_types_supported",
    "scopes_supported",
    "token_endpoint",
    "token_endpoint_auth_methods_supported",
  ]);
});

test("protectedResourceEdgeMetadata echoes the supplied resource (root vs /mcp)", () => {
  const id = createOAuthIdentity("https://herdr-mcp.example.com");
  const root = protectedResourceEdgeMetadata(id, id.issuer);
  assert.deepEqual(root, {
    resource: "https://herdr-mcp.example.com",
    authorization_servers: ["https://herdr-mcp.example.com"],
    scopes_supported: ["mcp"],
    bearer_methods_supported: ["header"],
    resource_name: "herdr-mcp",
  });
  const pathAware = protectedResourceEdgeMetadata(id, id.resource);
  assert.equal(pathAware.resource, "https://herdr-mcp.example.com/mcp");
  assert.deepEqual(pathAware.authorization_servers, [id.issuer]);
});

test("mcpServerCardMetadata requires serverUrl = resource", () => {
  const id = createOAuthIdentity("https://herdr-mcp.example.com");
  const card = mcpServerCardMetadata(id, "herdr-mcp", "0.3.23");
  assert.deepEqual(card, {
    serverUrl: "https://herdr-mcp.example.com/mcp",
    name: "herdr-mcp",
    version: "0.3.23",
  });
});

// ---------------------------------------------------------------------------
// JWT access-token validation
// ---------------------------------------------------------------------------

test("JWT: valid token with client_id claim is accepted statelessly", async (t) => {
  const issuer = "https://herdr-mcp.example.com";
  const id = createOAuthIdentity(issuer);
  const { publicKey, privateKey } = await generateRsaKeypair();
  const now = 1_700_000_000;
  const token = await signJwt(
    { iss: issuer, aud: id.resource, sub: "client-abc", client_id: "client-abc", scope: "mcp", iat: now, exp: now + 3600, jti: "j1" },
    privateKey,
  );
  const verifier = createRs256AccessTokenVerifier(id, publicKey);
  const verdict = await verifier.verify(token, now);
  assert.equal(verdict.ok, true);
  assert.equal(verdict.clientId, "client-abc");

  // resolveAccessToken end-to-end (stateless path):
  const info = await resolveAccessToken(token, {
    identity: id,
    verifier,
    opaqueStore: { get: () => undefined, delete: () => {} },
    nowSec: now,
  });
  assert.equal(info.ok, true);
  assert.equal(info.clientId, "client-abc");
});

test("JWT: sub is used as clientId when client_id claim absent", async (t) => {
  const issuer = "https://herdr-mcp.example.com";
  const id = createOAuthIdentity(issuer);
  const { publicKey, privateKey } = await generateRsaKeypair();
  const now = 1_700_000_000;
  const token = await signJwt(
    { iss: issuer, aud: id.resource, sub: "sub-only-client", iat: now, exp: now + 3600 },
    privateKey,
  );
  const verdict = await createRs256AccessTokenVerifier(id, publicKey).verify(token, now);
  assert.equal(verdict.ok, true);
  assert.equal(verdict.clientId, "sub-only-client");
});

test("JWT: aud accepted as issuer, resource, or array containing either", async (t) => {
  const issuer = "https://herdr-mcp.example.com";
  const id = createOAuthIdentity(issuer);
  const { publicKey, privateKey } = await generateRsaKeypair();
  const now = 1_700_000_000;
  const verifier = createRs256AccessTokenVerifier(id, publicKey);

  for (const aud of [id.resource, id.issuer, [id.resource], [id.issuer], [id.issuer, "other"]]) {
    const token = await signJwt({ iss: issuer, aud, sub: "c1", iat: now, exp: now + 3600 }, privateKey);
    const verdict = await verifier.verify(token, now);
    assert.equal(verdict.ok, true, `aud should be accepted: ${JSON.stringify(aud)}`);
  }
});

test("JWT: wrong issuer is unverifiable (falls through to opaque fallback)", async (t) => {
  const id = createOAuthIdentity("https://herdr-mcp.example.com");
  const { publicKey, privateKey } = await generateRsaKeypair();
  const now = 1_700_000_000;
  // Signed with our key but for a different issuer.
  const token = await signJwt(
    { iss: "https://evil.example.com", aud: id.resource, sub: "c1", iat: now, exp: now + 3600 },
    privateKey,
  );
  const verifier = createRs256AccessTokenVerifier(id, publicKey);
  await assert.rejects(() => verifier.verify(token, now), /issuer mismatch/);

  // Unverifiable → opaque fallback consulted → miss → gone.
  let deleted = false;
  const info = await resolveAccessToken(token, {
    identity: id,
    verifier,
    opaqueStore: { get: () => undefined, delete: () => { deleted = true; } },
    nowSec: now,
  });
  assert.equal(info.ok, false);
  assert.equal(deleted, false);
});

test("JWT: missing or null aud is REJECTED (audOk computed unconditionally, no opaque fallback)", async (t) => {
  const issuer = "https://herdr-mcp.example.com";
  const id = createOAuthIdentity(issuer);
  const { publicKey, privateKey } = await generateRsaKeypair();
  const now = 1_700_000_000;
  const verifier = createRs256AccessTokenVerifier(id, publicKey);

  // src/oauth.ts computes audOk unconditionally, so a JWT with NO `aud` claim
  // is audOk=false → semantic rejection (never falls through to opaque).
  const missingAud = await signJwt({ iss: issuer, sub: "c1", iat: now, exp: now + 3600 }, privateKey);
  let opaqueCalled = false;
  const info = await resolveAccessToken(missingAud, {
    identity: id,
    verifier,
    opaqueStore: { get: () => { opaqueCalled = true; return undefined; }, delete: () => {} },
    nowSec: now,
  });
  assert.equal(info.ok, false);
  assert.equal(opaqueCalled, false, "missing-aud JWT must be rejected without opaque fallback");

  // aud: null behaves identically to missing (audOk false).
  const nullAud = await signJwt({ iss: issuer, aud: null, sub: "c1", iat: now, exp: now + 3600 }, privateKey);
  assert.deepEqual(await verifier.verify(nullAud, now), { ok: false, kind: "rejected", reason: "audience mismatch" });
});

test("JWT: foreign audience is REJECTED without opaque fallback", async (t) => {
  const issuer = "https://herdr-mcp.example.com";
  const id = createOAuthIdentity(issuer);
  const { publicKey, privateKey } = await generateRsaKeypair();
  const now = 1_700_000_000;
  const token = await signJwt(
    { iss: issuer, aud: "https://some-other-rs.example.com/mcp", sub: "c1", iat: now, exp: now + 3600 },
    privateKey,
  );
  // Verifier rejects (not throws) → resolveAccessToken must NOT consult opaque.
  let opaqueCalled = false;
  const info = await resolveAccessToken(token, {
    identity: id,
    verifier: createRs256AccessTokenVerifier(id, publicKey),
    opaqueStore: {
      get: () => { opaqueCalled = true; return undefined; },
      delete: () => {},
    },
    nowSec: now,
  });
  assert.equal(info.ok, false);
  assert.equal(opaqueCalled, false, "rejected JWT must not fall through to opaque");
});

test("JWT: expired token rejected at the exp <= now boundary (no opaque fallback)", async (t) => {
  const issuer = "https://herdr-mcp.example.com";
  const id = createOAuthIdentity(issuer);
  const { publicKey, privateKey } = await generateRsaKeypair();
  const now = 1_700_000_000;
  const token = await signJwt(
    { iss: issuer, aud: id.resource, sub: "c1", iat: now - 7200, exp: now }, // exp == now → invalid
    privateKey,
  );
  let opaqueCalled = false;
  const info = await resolveAccessToken(token, {
    identity: id,
    verifier: createRs256AccessTokenVerifier(id, publicKey),
    opaqueStore: { get: () => { opaqueCalled = true; return undefined; }, delete: () => {} },
    nowSec: now,
  });
  assert.equal(info.ok, false);
  assert.equal(opaqueCalled, false);

  const future = await signJwt(
    { iss: issuer, aud: id.resource, sub: "c2", iat: now, exp: now + 1 },
    privateKey,
  );
  const ok = await resolveAccessToken(future, {
    identity: id,
    verifier: createRs256AccessTokenVerifier(id, publicKey),
    opaqueStore: { get: () => undefined, delete: () => {} },
    nowSec: now,
  });
  assert.equal(ok.ok, true);
});

test("JWT: future nbf rejected; non-numeric exp/nbf throw (unverifiable)", async (t) => {
  const issuer = "https://herdr-mcp.example.com";
  const id = createOAuthIdentity(issuer);
  const { publicKey, privateKey } = await generateRsaKeypair();
  const now = 1_700_000_000;
  const verifier = createRs256AccessTokenVerifier(id, publicKey);

  const notYet = await signJwt(
    { iss: issuer, aud: id.resource, sub: "c1", iat: now, exp: now + 3600, nbf: now + 100 },
    privateKey,
  );
  assert.deepEqual(await verifier.verify(notYet, now), { ok: false, kind: "rejected", reason: "token not yet valid" });

  const badExp = await signJwt({ iss: issuer, aud: id.resource, sub: "c2", exp: "soon" }, privateKey);
  await assert.rejects(() => verifier.verify(badExp, now), /malformed exp/);
  const badNbf = await signJwt({ iss: issuer, aud: id.resource, sub: "c3", nbf: "later" }, privateKey);
  await assert.rejects(() => verifier.verify(badNbf, now), /malformed nbf/);
});

test("JWT: tampered signature / malformed token / wrong alg all throw → unverifiable", async (t) => {
  const issuer = "https://herdr-mcp.example.com";
  const id = createOAuthIdentity(issuer);
  const { publicKey, privateKey } = await generateRsaKeypair();
  const now = 1_700_000_000;
  const verifier = createRs256AccessTokenVerifier(id, publicKey);

  const good = await signJwt({ iss: issuer, aud: id.resource, sub: "c1", iat: now, exp: now + 3600 }, privateKey);
  // Deterministic tamper: same header, valid-JSON payload with a DIFFERENT claim
  // (extra scope), reusing the ORIGINAL signature → guaranteed invalid signature
  // while remaining 3 segments / parseable (no random last-char collision).
  const [hdrTwo, , sig] = good.split(".");
  const tamperedPayload = base64urlEncode(
    enc.encode(JSON.stringify({ iss: issuer, aud: id.resource, sub: "c1", iat: now, exp: now + 3600, scope: "tampered" })),
  );
  await assert.rejects(async () => verifier.verify(`${hdrTwo}.${tamperedPayload}.${sig}`, now), /signature/);
  await assert.rejects(async () => verifier.verify("only.two.parts.extra", now), /3 segments/);
  await assert.rejects(async () => verifier.verify("abc.def.ghi", now), /malformed access token/);
  await assert.rejects(async () => verifier.verify("###.###.###", now), /malformed access token|invalid base64url/);

  const hs256 = await signJwt({ iss: issuer, aud: id.resource, sub: "c1" }, privateKey, { alg: "HS256" });
  await assert.rejects(async () => verifier.verify(hs256, now), /algorithm/);

  // All unverifiable tokens fall through to opaque store.
  let opaqueCalls = 0;
  const info = await resolveAccessToken("abc.def.ghi", {
    identity: id,
    verifier,
    opaqueStore: { get: () => { opaqueCalls++; return undefined; }, delete: () => {} },
    nowSec: now,
  });
  assert.equal(info.ok, false);
  assert.equal(opaqueCalls, 1);
});

test("JWT: path disabled when no verifier configured (opaque-only, like missing key)", async (t) => {
  const id = createOAuthIdentity("https://herdr-mcp.example.com");
  const hash = await hashOpaqueToken("opaque-token-1");
  const info = await resolveAccessToken("has.dot.but.no-verifier", {
    identity: id,
    verifier: null,
    opaqueStore: {
      get: (h) => (h === hash ? { client_id: "legacy-client", resource: id.resource, scope: "mcp", expires_at: 1_700_000_000 + 99 } : undefined),
      delete: () => {},
    },
    nowSec: 1_700_000_000,
  });
  // JWT path disabled entirely → falls straight to opaque (miss on this token).
  assert.equal(info.ok, false);
});

// ---------------------------------------------------------------------------
// Opaque legacy access tokens
// ---------------------------------------------------------------------------

test("opaque: valid token resolves from store with clientId; hashing matches src format", async (t) => {
  const id = createOAuthIdentity("https://herdr-mcp.example.com");
  const raw = "opaque-access-token";
  const hash = await hashOpaqueToken(raw);
  // src format: sha256("herdr-mcp-oauth:" + token), hex — verified by the
  // decode round-trip below matching an independently computed digest.
  const expected = await crypto.subtle.digest("SHA-256", enc.encode(`herdr-mcp-oauth:${raw}`));
  assert.equal(hash, Array.from(new Uint8Array(expected)).map((b) => b.toString(16).padStart(2, "0")).join(""));

  const info = await resolveAccessToken(raw, {
    identity: id,
    verifier: null,
    opaqueStore: {
      get: (h) => (h === hash ? { client_id: "opaque-client", resource: id.resource, scope: "mcp", expires_at: 1_700_000_000 + 99 } : undefined),
      delete: () => {},
    },
    nowSec: 1_700_000_000,
  });
  assert.equal(info.ok, true);
  assert.equal(info.clientId, "opaque-client");
});

test("opaque: unknown token rejected; expired token deleted and rejected", async (t) => {
  const id = createOAuthIdentity("https://herdr-mcp.example.com");
  const now = 1_700_000_000;

  const unknown = await resolveAccessToken("unknown-token", {
    identity: id,
    verifier: null,
    opaqueStore: { get: () => undefined, delete: () => {} },
    nowSec: now,
  });
  assert.equal(unknown.ok, false);

  // Expiry asymmetry: opaque only expires when now > expires_at (== is still valid).
  const hash = await hashOpaqueToken("boundary-token");
  const atBoundary = await resolveAccessToken("boundary-token", {
    identity: id,
    verifier: null,
    opaqueStore: {
      get: (h) => (h === hash ? { client_id: "c", resource: id.resource, scope: "mcp", expires_at: now } : undefined),
      delete: () => {},
    },
    nowSec: now,
  });
  assert.equal(atBoundary.ok, true, "opaque token expiring exactly now is still valid (JWT would reject)");

  let deleted = false;
  let deletedHash = null;
  const expired = await resolveAccessToken("expired-token", {
    identity: id,
    verifier: null,
    opaqueStore: {
      get: () => ({ client_id: "c", resource: id.resource, scope: "mcp", expires_at: now - 1 }),
      delete: (h) => { deleted = true; deletedHash = h; },
    },
    nowSec: now,
  });
  assert.equal(expired.ok, false);
  assert.equal(deleted, true, "expired opaque record must be deleted like src/oauth.ts");
  assert.equal(deletedHash, await hashOpaqueToken("expired-token"));
});

test("resolveAccessToken: JWT failure falls through to opaque when opaque has the token", async (t) => {
  // Expired JWT fails all paths here, but a *structurally malformed* JWT-shaped
  // token that happens to match an opaque hash must still resolve via opaque.
  const id = createOAuthIdentity("https://herdr-mcp.example.com");
  // base64url cannot contain '.'; craft a 3-segment junk token and seed the
  // opaque store with its hash.
  const junk = "AAAA.BBBB.CCCC";
  const hash = await hashOpaqueToken(junk);
  const info = await resolveAccessToken(junk, {
    identity: id,
    verifier: createRs256AccessTokenVerifier(id, await (await generateRsaKeypair()).publicKey),
    opaqueStore: {
      get: (h) => (h === hash ? { client_id: "opaque-client", resource: id.resource, scope: "mcp", expires_at: 1_700_000_000 + 999 } : undefined),
      delete: () => {},
    },
    nowSec: 1_700_000_000,
  });
  assert.equal(info.ok, true);
  assert.equal(info.clientId, "opaque-client");
});

// ---------------------------------------------------------------------------
// Client / refresh registry normalization (state that still needs import)
// ---------------------------------------------------------------------------

test("normalizeStoredClient mirrors loadAll defaults; invalid records dropped", () => {
  const now = 1_700_000_000;
  const ok = normalizeStoredClient("dcr-abc", {
    client_secret_hash: "deadbeef",
    redirect_uris: ["https://chatgpt.com/connector_platform_oauth_redirect"],
    token_endpoint_auth_method: "client_secret_post",
    grant_types: ["authorization_code", "refresh_token"],
    scope: "mcp",
    client_name: "ChatGPT",
    issued_at: now,
  }, now);
  assert.deepEqual(ok, {
    client_secret_hash: "deadbeef",
    redirect_uris: ["https://chatgpt.com/connector_platform_oauth_redirect"],
    token_endpoint_auth_method: "client_secret_post",
    grant_types: ["authorization_code", "refresh_token"],
    scope: "mcp",
    client_name: "ChatGPT",
    issued_at: now,
  });

  // Defaults when fields missing.
  const defaults = normalizeStoredClient("dcr-default", { redirect_uris: ["https://x/redir"], issued_at: now }, now);
  assert.equal(defaults.client_secret_hash, null);
  assert.equal(defaults.token_endpoint_auth_method, "none");
  assert.deepEqual(defaults.grant_types, ["authorization_code", "refresh_token"]);
  assert.equal(defaults.scope, "mcp");

  // Invalid: missing id / missing redirect_uris.
  assert.equal(normalizeStoredClient("", { redirect_uris: [] }, now), null);
  assert.equal(normalizeStoredClient("dcr-bad", { scope: "mcp" }, now), null);

  // IMPORTANT: registry is required — clients are NOT stateless; only the
  // secret HASH is present, never the raw secret.
  assert.ok(!("client_secret" in ok));
});

test("normalizeStoredToken filters expired/invalid and defaults resource/scope", () => {
  const now = 1_700_000_000;
  const id = createOAuthIdentity("https://herdr-mcp.example.com");
  const ok = normalizeStoredToken({ client_id: "c1", expires_at: now + 100 }, now, id);
  assert.deepEqual(ok, { client_id: "c1", resource: id.resource, scope: "mcp", expires_at: now + 100 });

  assert.equal(normalizeStoredToken({ client_id: "c2", expires_at: now }, now, id), null, "expires_at <= now dropped");
  assert.equal(normalizeStoredToken({ expires_at: now + 100 }, now, id), null, "missing client_id dropped");
  assert.equal(normalizeStoredToken({ client_id: "c3", expires_at: "later" }, now, id), null, "non-numeric expires_at dropped");

  const explicitResource = normalizeStoredToken(
    { client_id: "c4", resource: "https://custom/mcp", scope: "wide", expires_at: now + 100 },
    now,
    id,
  );
  assert.equal(explicitResource.resource, "https://custom/mcp");
  assert.equal(explicitResource.scope, "wide");
});