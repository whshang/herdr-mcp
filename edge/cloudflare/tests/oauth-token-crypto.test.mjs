import { test } from "node:test";
import assert from "node:assert/strict";
import {
  importRs256PrivateKeyPem,
  importRs256PublicKeyPem,
  issueRs256AccessJwt,
  verifyChatgptPrivateKeyJwt,
  s256Challenge,
  verifyPkceS256,
  PKCE_VERIFIER_RE,
  randomBase64UrlToken,
  isChatgptOAuthClientId,
} from "../dist/oauth-token-crypto.js";
import {
  createOAuthIdentity,
  createRs256AccessTokenVerifier,
  base64urlDecode,
} from "../dist/oauth-edge.js";

// ---------------------------------------------------------------------------
// Test helpers: PEM generation, JWK export, compact JWT signing (all Web Crypto)
// ---------------------------------------------------------------------------

const enc = new TextEncoder();

function b64urlEncode(bytes) {
  let s = "";
  for (const b of bytes) s += String.fromCharCode(b);
  return btoa(s).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

function toBase64(bytes) {
  let s = "";
  for (const b of bytes) s += String.fromCharCode(b);
  return btoa(s);
}

function pemWrap(der, label) {
  const b64 = toBase64(der);
  const lines = b64.match(/.{1,64}/g) ?? [b64];
  return `-----BEGIN ${label}-----\n${lines.join("\n")}\n-----END ${label}-----\n`;
}

async function generateRsaKeyPair() {
  return crypto.subtle.generateKey(
    { name: "RSASSA-PKCS1-v1_5", modulusLength: 2048, publicExponent: new Uint8Array([1, 0, 1]), hash: "SHA-256" },
    true,
    ["sign", "verify"],
  );
}

async function exportPems(keypair) {
  const priv = await crypto.subtle.exportKey("pkcs8", keypair.privateKey);
  const pub = await crypto.subtle.exportKey("spki", keypair.publicKey);
  return { privPem: pemWrap(new Uint8Array(priv), "PRIVATE KEY"), pubPem: pemWrap(new Uint8Array(pub), "PUBLIC KEY") };
}

async function exportJwk(publicKey, kid, alg = "RS256") {
  const jwk = await crypto.subtle.exportKey("jwk", publicKey);
  return { ...jwk, alg, ...(kid ? { kid } : {}) };
}

async function signCompact(header, payload, privateKey) {
  const h = b64urlEncode(enc.encode(JSON.stringify(header)));
  const p = b64urlEncode(enc.encode(JSON.stringify(payload)));
  const data = enc.encode(`${h}.${p}`);
  const sig = await crypto.subtle.sign("RSASSA-PKCS1-v1_5", privateKey, data);
  return `${h}.${p}.${b64urlEncode(new Uint8Array(sig))}`;
}

function makeFetch(jwks) {
  return async (_url) => {
    const body = JSON.stringify({ keys: jwks });
    return new Response(body, { status: 200, headers: { "content-type": "application/json" } });
  };
}

const ISSUER = "https://herdr-mcp.example.com";
const CLIENT_ID = "https://chatgpt.com/oauth/connector/client/abc123";
const OAUTH_TOKEN_URL = `${ISSUER}/oauth/token`;

function clientAssertionPayload(clientId = CLIENT_ID, aud = OAUTH_TOKEN_URL, nowSec = 1_700_000_000) {
  return { iss: clientId, aud, sub: clientId, iat: nowSec, exp: nowSec + 300 };
}

// ---------------------------------------------------------------------------
// 1. PEM key import (pure Web Crypto, no node:crypto)
// ---------------------------------------------------------------------------

test("importRs256PrivateKeyPem / importRs256PublicKeyPem round-trip", async () => {
  const kp = await generateRsaKeyPair();
  const { privPem, pubPem } = await exportPems(kp);
  const priv = await importRs256PrivateKeyPem(privPem);
  const pub = await importRs256PublicKeyPem(pubPem);
  assert.equal(priv.type, "private");
  assert.equal(pub.type, "public");
  // Sign with imported private key, verify with imported public key.
  const data = enc.encode("hello");
  const sig = new Uint8Array(await crypto.subtle.sign("RSASSA-PKCS1-v1_5", priv, data));
  assert.equal(await crypto.subtle.verify("RSASSA-PKCS1-v1_5", pub, sig, data), true);
});

test("import rejects malformed PEM labels", async () => {
  const kp = await generateRsaKeyPair();
  const { pubPem } = await exportPems(kp);
  // PUBLIC KEY label fed to private-key import -> label mismatch -> reject.
  await assert.rejects(() => importRs256PrivateKeyPem(pubPem), /invalid PRIVATE KEY PEM/);
  // Garbage input
  await assert.rejects(() => importRs256PublicKeyPem("not a pem"), /invalid PUBLIC KEY PEM/);
  await assert.rejects(() => importRs256PublicKeyPem(
    "-----BEGIN PUBLIC KEY-----\n!!!\n-----END PUBLIC KEY-----",
  ), /base64 character/);
});

// ---------------------------------------------------------------------------
// 2. JWT issuance (src/oauth.ts claims/header match)
// ---------------------------------------------------------------------------

test("issueRs256AccessJwt embeds exact claims and header", async () => {
  const kp = await generateRsaKeyPair();
  const now = 1_700_000_000;
  const resource = `${ISSUER}/mcp`;
  const token = await issueRs256AccessJwt(kp.privateKey, ISSUER, resource, "client-xyz", 3600, {
    jti: "jti-fixed",
    nowSec: now,
  });
  const [hB64, pB64] = token.split(".");
  const header = JSON.parse(new TextDecoder().decode(base64urlDecode(hB64)));
  const payload = JSON.parse(new TextDecoder().decode(base64urlDecode(pB64)));
  // Header: alg RS256, typ at+jwt
  assert.deepEqual(header, { alg: "RS256", typ: "at+jwt" });
  // Claims: client_id, scope mcp, iss, aud, sub, jti, iat, exp
  assert.equal(payload.client_id, "client-xyz");
  assert.equal(payload.scope, "mcp");
  assert.equal(payload.iss, ISSUER);
  assert.equal(payload.aud, resource);
  assert.equal(payload.sub, "client-xyz");
  assert.equal(payload.jti, "jti-fixed");
  assert.equal(payload.iat, now);
  assert.equal(payload.exp, now + 3600);
});

test("issueRs256AccessJwt auto-generates jti when not supplied", async () => {
  const kp = await generateRsaKeyPair();
  const t1 = await issueRs256AccessJwt(kp.privateKey, ISSUER, "r", "c", 60, { nowSec: 1_700_000_000 });
  const t2 = await issueRs256AccessJwt(kp.privateKey, ISSUER, "r", "c", 60, { nowSec: 1_700_000_000 });
  const p1 = JSON.parse(new TextDecoder().decode(base64urlDecode(t1.split(".")[1])));
  const p2 = JSON.parse(new TextDecoder().decode(base64urlDecode(t2.split(".")[1])));
  assert.ok(p1.jti && p1.jti.length >= 16);
  assert.notEqual(p1.jti, p2.jti);
});

test("issued JWT verifies with createRs256AccessTokenVerifier (edge compat)", async () => {
  const kp = await generateRsaKeyPair();
  const { pubPem } = await exportPems(kp);
  const pub = await importRs256PublicKeyPem(pubPem);
  const identity = createOAuthIdentity(ISSUER);
  const verifier = createRs256AccessTokenVerifier(identity, pub);

  const token = await issueRs256AccessJwt(kp.privateKey, ISSUER, identity.resource, "dcr-client-1", 3600, {
    nowSec: 1_700_000_000,
  });
  const verdict = await verifier.verify(token, 1_700_000_000);
  assert.deepEqual(verdict, { ok: true, clientId: "dcr-client-1" });
});

// ---------------------------------------------------------------------------
// 3. private_key_jwt assertion verification (ChatGPT CIMD)
// ---------------------------------------------------------------------------

test("assertion: valid RS256 assertion with kid accepted", async () => {
  const kp = await generateRsaKeyPair();
  const jwk = await exportJwk(kp.publicKey, "kid-1");
  const now = 1_700_000_000;
  const assertion = await signCompact(
    { alg: "RS256", kid: "kid-1", typ: "client-authentication+jwt" },
    clientAssertionPayload(CLIENT_ID, OAUTH_TOKEN_URL, now),
    kp.privateKey,
  );
  const verdict = await verifyChatgptPrivateKeyJwt(assertion, CLIENT_ID, ISSUER, now, makeFetch([jwk]));
  assert.deepEqual(verdict, { ok: true, clientId: CLIENT_ID });
});

test("assertion: kid matching; JWK without alg still usable", async () => {
  const kp = await generateRsaKeyPair();
  const jwk = await exportJwk(kp.publicKey, "kid-2");
  delete jwk.alg; // some JWKS omit alg; still selectable by kid
  const now = 1_700_000_000;
  const assertion = await signCompact(
    { alg: "RS256", kid: "kid-2" },
    clientAssertionPayload(CLIENT_ID, OAUTH_TOKEN_URL, now),
    kp.privateKey,
  );
  const verdict = await verifyChatgptPrivateKeyJwt(assertion, CLIENT_ID, ISSUER, now, makeFetch([jwk]));
  assert.deepEqual(verdict, { ok: true, clientId: CLIENT_ID });
});

test("assertion: bad signature -> bad_signature", async () => {
  const kp = await generateRsaKeyPair();
  const jwk = await exportJwk(kp.publicKey, "kid-1");
  const now = 1_700_000_000;
  // Sign with a different key -> signature invalid
  const other = await generateRsaKeyPair();
  const assertion = await signCompact(
    { alg: "RS256", kid: "kid-1" },
    clientAssertionPayload(CLIENT_ID, OAUTH_TOKEN_URL, now),
    other.privateKey,
  );
  const verdict = await verifyChatgptPrivateKeyJwt(assertion, CLIENT_ID, ISSUER, now, makeFetch([jwk]));
  assert.deepEqual(verdict, { ok: false, code: "bad_signature" });
});

test("assertion: wrong kid -> bad_kid", async () => {
  const kp = await generateRsaKeyPair();
  const jwk = await exportJwk(kp.publicKey, "kid-1");
  const now = 1_700_000_000;
  const assertion = await signCompact(
    { alg: "RS256", kid: "missing-kid" },
    clientAssertionPayload(CLIENT_ID, OAUTH_TOKEN_URL, now),
    kp.privateKey,
  );
  const verdict = await verifyChatgptPrivateKeyJwt(assertion, CLIENT_ID, ISSUER, now, makeFetch([jwk]));
  assert.deepEqual(verdict, { ok: false, code: "bad_kid" });
});

test("assertion: non-RS256 alg in JWK disallows kid match", async () => {
  const kp = await generateRsaKeyPair();
  const jwk = await exportJwk(kp.publicKey, "kid-1", "RS512");
  const now = 1_700_000_000;
  const assertion = await signCompact(
    { alg: "RS256", kid: "kid-1" },
    clientAssertionPayload(CLIENT_ID, OAUTH_TOKEN_URL, now),
    kp.privateKey,
  );
  // Only RS512 key in JWKS -> no RS256 key matches kid -> bad_kid
  const verdict = await verifyChatgptPrivateKeyJwt(assertion, CLIENT_ID, ISSUER, now, makeFetch([jwk]));
  assert.equal(verdict.ok, false);
  assert.equal(verdict.code, "bad_kid");
});

test("assertion: wrong issuer -> bad_issuer", async () => {
  const kp = await generateRsaKeyPair();
  const jwk = await exportJwk(kp.publicKey, "kid-1");
  const now = 1_700_000_000;
  const assertion = await signCompact(
    { alg: "RS256", kid: "kid-1" },
    clientAssertionPayload(CLIENT_ID, OAUTH_TOKEN_URL, now),
    kp.privateKey,
  );
  // Re-sign with tampered iss
  const tampered = await signCompact(
    { alg: "RS256", kid: "kid-1" },
    { ...clientAssertionPayload(CLIENT_ID, OAUTH_TOKEN_URL, now), iss: "https://evil.example.com" },
    kp.privateKey,
  );
  const verdict = await verifyChatgptPrivateKeyJwt(tampered, CLIENT_ID, ISSUER, now, makeFetch([jwk]));
  assert.deepEqual(verdict, { ok: false, code: "bad_issuer" });
});

test("assertion: wrong aud -> bad_audience", async () => {
  const kp = await generateRsaKeyPair();
  const jwk = await exportJwk(kp.publicKey, "kid-1");
  const now = 1_700_000_000;
  const assertion = await signCompact(
    { alg: "RS256", kid: "kid-1" },
    clientAssertionPayload(CLIENT_ID, "https://evil/oauth/token", now),
    kp.privateKey,
  );
  const verdict = await verifyChatgptPrivateKeyJwt(assertion, CLIENT_ID, ISSUER, now, makeFetch([jwk]));
  assert.deepEqual(verdict, { ok: false, code: "bad_audience" });
});

test("assertion: aud accepts bare issuer and array forms", async () => {
  const kp = await generateRsaKeyPair();
  const jwk = await exportJwk(kp.publicKey, "kid-1");
  const now = 1_700_000_000;
  const byIssuer = await signCompact(
    { alg: "RS256", kid: "kid-1" },
    clientAssertionPayload(CLIENT_ID, ISSUER, now),
    kp.privateKey,
  );
  const byArray = await signCompact(
    { alg: "RS256", kid: "kid-1" },
    { ...clientAssertionPayload(CLIENT_ID, OAUTH_TOKEN_URL, now), aud: [OAUTH_TOKEN_URL, "other"] },
    kp.privateKey,
  );
  assert.deepEqual(
    await verifyChatgptPrivateKeyJwt(byIssuer, CLIENT_ID, ISSUER, now, makeFetch([jwk])),
    { ok: true, clientId: CLIENT_ID },
  );
  assert.deepEqual(
    await verifyChatgptPrivateKeyJwt(byArray, CLIENT_ID, ISSUER, now, makeFetch([jwk])),
    { ok: true, clientId: CLIENT_ID },
  );
});

test("assertion: mismatched sub -> bad_sub; missing sub ok", async () => {
  const kp = await generateRsaKeyPair();
  const jwk = await exportJwk(kp.publicKey, "kid-1");
  const now = 1_700_000_000;
  const badSub = await signCompact(
    { alg: "RS256", kid: "kid-1" },
    { ...clientAssertionPayload(CLIENT_ID, OAUTH_TOKEN_URL, now), sub: "https://chatgpt.com/other" },
    kp.privateKey,
  );
  assert.deepEqual(
    await verifyChatgptPrivateKeyJwt(badSub, CLIENT_ID, ISSUER, now, makeFetch([jwk])),
    { ok: false, code: "bad_sub" },
  );
  // No sub claim -> ok
  const noSub = await signCompact(
    { alg: "RS256", kid: "kid-1" },
    { iss: CLIENT_ID, aud: OAUTH_TOKEN_URL, iat: now, exp: now + 300 },
    kp.privateKey,
  );
  assert.deepEqual(
    await verifyChatgptPrivateKeyJwt(noSub, CLIENT_ID, ISSUER, now, makeFetch([jwk])),
    { ok: true, clientId: CLIENT_ID },
  );
});

test("assertion: expiry checks (expired, not-yet-valid, valid)", async () => {
  const kp = await generateRsaKeyPair();
  const jwk = await exportJwk(kp.publicKey, "kid-1");
  const now = 1_700_000_000;
  const expired = await signCompact(
    { alg: "RS256", kid: "kid-1" },
    { ...clientAssertionPayload(CLIENT_ID, OAUTH_TOKEN_URL, now), exp: now },
    kp.privateKey,
  );
  assert.deepEqual(
    await verifyChatgptPrivateKeyJwt(expired, CLIENT_ID, ISSUER, now, makeFetch([jwk])),
    { ok: false, code: "expired" },
  );
  const notYet = await signCompact(
    { alg: "RS256", kid: "kid-1" },
    { ...clientAssertionPayload(CLIENT_ID, OAUTH_TOKEN_URL, now), nbf: now + 10 },
    kp.privateKey,
  );
  assert.deepEqual(
    await verifyChatgptPrivateKeyJwt(notYet, CLIENT_ID, ISSUER, now, makeFetch([jwk])),
    { ok: false, code: "not_yet_valid" },
  );
  const valid = await signCompact(
    { alg: "RS256", kid: "kid-1" },
    { ...clientAssertionPayload(CLIENT_ID, OAUTH_TOKEN_URL, now), nbf: now - 5 },
    kp.privateKey,
  );
  assert.deepEqual(
    await verifyChatgptPrivateKeyJwt(valid, CLIENT_ID, ISSUER, now, makeFetch([jwk])),
    { ok: true, clientId: CLIENT_ID },
  );
});

test("assertion: non-chatgpt host / non-https / non-URL clientId -> malformed", async () => {
  const kp = await generateRsaKeyPair();
  const jwk = await exportJwk(kp.publicKey, "kid-1");
  const now = 1_700_000_000;
  const assertion = await signCompact(
    { alg: "RS256", kid: "kid-1" },
    clientAssertionPayload(CLIENT_ID, OAUTH_TOKEN_URL, now),
    kp.privateKey,
  );
  // Non-https URL
  assert.deepEqual(
    await verifyChatgptPrivateKeyJwt(assertion, "http://chatgpt.com/oauth/client", ISSUER, now, makeFetch([jwk])),
    { ok: false, code: "malformed" },
  );
  // Non-chatgpt host
  assert.deepEqual(
    await verifyChatgptPrivateKeyJwt(assertion, "https://evil.example.com/oauth/client", ISSUER, now, makeFetch([jwk])),
    { ok: false, code: "malformed" },
  );
  // Non-URL string
  assert.deepEqual(
    await verifyChatgptPrivateKeyJwt(assertion, "chatgpt.com", ISSUER, now, makeFetch([jwk])),
    { ok: false, code: "malformed" },
  );
});

test("assertion: www.chatgpt.com host accepted", async () => {
  const kp = await generateRsaKeyPair();
  const jwk = await exportJwk(kp.publicKey, "kid-1");
  const now = 1_700_000_000;
  const wwwClient = "https://www.chatgpt.com/oauth/client/xyz";
  const assertion = await signCompact(
    { alg: "RS256", kid: "kid-1" },
    clientAssertionPayload(wwwClient, OAUTH_TOKEN_URL, now),
    kp.privateKey,
  );
  const verdict = await verifyChatgptPrivateKeyJwt(assertion, wwwClient, ISSUER, now, makeFetch([jwk]));
  assert.deepEqual(verdict, { ok: true, clientId: wwwClient });
});

test("assertion: JWKS fetch failure -> bad_jwks_fetch (network / HTTP error)", async () => {
  const kp = await generateRsaKeyPair();
  const now = 1_700_000_000;
  const assertion = await signCompact(
    { alg: "RS256", kid: "kid-1" },
    clientAssertionPayload(CLIENT_ID, OAUTH_TOKEN_URL, now),
    kp.privateKey,
  );
  // Network throws
  assert.deepEqual(
    await verifyChatgptPrivateKeyJwt(assertion, CLIENT_ID, ISSUER, now, async () => { throw new Error("network down"); }),
    { ok: false, code: "bad_jwks_fetch" },
  );
  // HTTP error status
  assert.deepEqual(
    await verifyChatgptPrivateKeyJwt(assertion, CLIENT_ID, ISSUER, now, async () => new Response("", { status: 500 })),
    { ok: false, code: "bad_jwks_fetch" },
  );
});

test("assertion: oversized JWKS body -> bad_jwks_size; too many keys -> bad_jwks_count", async () => {
  const kp = await generateRsaKeyPair();
  const now = 1_700_000_000;
  const assertion = await signCompact(
    { alg: "RS256", kid: "kid-1" },
    clientAssertionPayload(CLIENT_ID, OAUTH_TOKEN_URL, now),
    kp.privateKey,
  );
  // Body > 64 KiB
  const hugeBody = "x".repeat(70000);
  assert.deepEqual(
    await verifyChatgptPrivateKeyJwt(assertion, CLIENT_ID, ISSUER, now, async () => new Response(hugeBody, { status: 200 })),
    { ok: false, code: "bad_jwks_size" },
  );
  // > 16 keys
  const manyKeys = Array.from({ length: 20 }, (_, i) => ({ kty: "RSA", alg: "RS256", kid: `k${i}`, n: "n", e: "AQAB" }));
  assert.deepEqual(
    await verifyChatgptPrivateKeyJwt(assertion, CLIENT_ID, ISSUER, now, async () => new Response(JSON.stringify({ keys: manyKeys }), { status: 200 })),
    { ok: false, code: "bad_jwks_count" },
  );
});

test("assertion: errors never echo assertion or key material", async () => {
  const kp = await generateRsaKeyPair();
  const jwk = await exportJwk(kp.publicKey, "kid-1");
  const now = 1_700_000_000;
  const assertion = await signCompact(
    { alg: "RS256", kid: "wrong-kid" },
    clientAssertionPayload(CLIENT_ID, OAUTH_TOKEN_URL, now),
    kp.privateKey,
  );
  const verdict = await verifyChatgptPrivateKeyJwt(assertion, CLIENT_ID, ISSUER, now, makeFetch([jwk]));
  assert.deepEqual(verdict, { ok: false, code: "bad_kid" });
  const serialized = JSON.stringify(verdict);
  assert.equal(serialized.includes(assertion), false);
  assert.equal(serialized.includes("chatgpt"), false);
  assert.equal(serialized.includes("kid-1"), false);
  assert.equal(serialized.includes("wrong-kid"), false);
});

test("assertion: malformed JWT segments", async () => {
  const now = 1_700_000_000;
  const v = await verifyChatgptPrivateKeyJwt("not.a.jwt", CLIENT_ID, ISSUER, now, makeFetch([]));
  assert.deepEqual(v, { ok: false, code: "malformed" });
  const v2 = await verifyChatgptPrivateKeyJwt("a.b", CLIENT_ID, ISSUER, now, makeFetch([]));
  assert.deepEqual(v2, { ok: false, code: "malformed" });
});

// ---------------------------------------------------------------------------
// 4. PKCE S256
// ---------------------------------------------------------------------------

test("PKCE: valid verifier produces matching challenge (constant-time verify)", async () => {
  const verifier = "aBc9-_~.az09".repeat(4); // 48 chars, matches regex
  assert.ok(PKCE_VERIFIER_RE.test(verifier));
  const challenge = await s256Challenge(verifier);
  assert.match(challenge, /^[A-Za-z0-9\-_]+$/);
  assert.equal(await verifyPkceS256(verifier, challenge), true);
});

test("PKCE: wrong challenge -> false; malformed verifier -> false", async () => {
  // "aBcDeFgHiJkLmNoPqRsTuVwXyZ" (26) + "0123456789" (10) + "-_.~" (4) = 40 — too short.
  // Pad to exactly 43 (within the 43-128 range) with unreserved chars.
  const verifier = "aBcDeFgHiJkLmNoPqRsTuVwXyZ0123456789-_.~abc"; // 43 chars
  assert.ok(PKCE_VERIFIER_RE.test(verifier));
  const challenge = await s256Challenge(verifier);
  // Wrong challenge
  assert.equal(await verifyPkceS256(verifier, challenge + "x"), false);
  // Too short
  assert.equal(await verifyPkceS256("tooshort", challenge), false);
  // Spaces
  assert.equal(await verifyPkceS256("bad verifier with spaces", challenge), false);
  // Too long
  assert.equal(await verifyPkceS256("a".repeat(200), challenge), false);
});

test("PKCE: challenge is base64url S256 of ASCII verifier (RFC 7636 §4.6)", async () => {
  const verifier = "0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJ";
  const challenge = await s256Challenge(verifier);
  // Independent recomputation
  const digest = await crypto.subtle.digest("SHA-256", enc.encode(verifier));
  const expected = b64urlEncode(new Uint8Array(digest));
  assert.equal(challenge, expected);
});

// ---------------------------------------------------------------------------
// 5. Token generation helpers
// ---------------------------------------------------------------------------

test("randomBase64UrlToken is unique and URL-safe", () => {
  const a = randomBase64UrlToken();
  const b = randomBase64UrlToken();
  assert.notEqual(a, b);
  assert.match(a, /^[A-Za-z0-9\-_]+$/);
  assert.ok(a.length >= 43);
});

test("isChatgptOAuthClientId matches src/oauth.ts semantics", () => {
  assert.equal(isChatgptOAuthClientId("https://chatgpt.com/oauth/client/1"), true);
  assert.equal(isChatgptOAuthClientId("https://www.chatgpt.com/oauth/client/1"), true);
  assert.equal(isChatgptOAuthClientId("https://evil.com/oauth/client/1"), false);
  assert.equal(isChatgptOAuthClientId("http://chatgpt.com/oauth/client/1"), false);
  assert.equal(isChatgptOAuthClientId("not-a-url"), false);
  assert.equal(isChatgptOAuthClientId(undefined), false);
  assert.equal(isChatgptOAuthClientId(""), false);
});