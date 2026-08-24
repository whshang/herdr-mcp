import { test } from "node:test";
import assert from "node:assert/strict";
import { authenticateMcpRequest, importRs256PublicKeyPem } from "../dist/oauth-mcp-auth.js";

const ISSUER = "https://herdr-mcp.agentforme.cc.cd";
const enc = new TextEncoder();

function b64url(bytes) {
  return Buffer.from(bytes).toString("base64url");
}

function request(token) {
  return new Request("https://edge.example/mcp", {
    headers: token ? { authorization: `Bearer ${token}` } : {},
  });
}

async function keyPair() {
  return crypto.subtle.generateKey(
    { name: "RSASSA-PKCS1-v1_5", modulusLength: 2048, publicExponent: new Uint8Array([1, 0, 1]), hash: "SHA-256" },
    true,
    ["sign", "verify"],
  );
}

async function publicPem(publicKey) {
  const der = new Uint8Array(await crypto.subtle.exportKey("spki", publicKey));
  const b64 = Buffer.from(der).toString("base64").match(/.{1,64}/g).join("\n");
  return `-----BEGIN PUBLIC KEY-----\n${b64}\n-----END PUBLIC KEY-----\n`;
}

async function jwt(privateKey, claims, header = { alg: "RS256", typ: "at+jwt" }) {
  const h = b64url(enc.encode(JSON.stringify(header)));
  const p = b64url(enc.encode(JSON.stringify(claims)));
  const signing = `${h}.${p}`;
  const signature = await crypto.subtle.sign("RSASSA-PKCS1-v1_5", privateKey, enc.encode(signing));
  return `${signing}.${b64url(new Uint8Array(signature))}`;
}

test("development bearer remains an explicit local compatibility fallback", async () => {
  const result = await authenticateMcpRequest(request("dev-secret"), { DEV_MCP_BEARER_SECRET: "dev-secret" });
  assert.deepEqual(result, { ok: true, source: "dev_bearer" });
});

test("static bearer preserves the current runtime operator/curl auth contract", async () => {
  const result = await authenticateMcpRequest(request("prod-static"), {
    STATIC_MCP_BEARER_SECRET: "prod-static",
  });
  assert.deepEqual(result, { ok: true, source: "static_bearer" });
  assert.deepEqual(
    await authenticateMcpRequest(request("wrong"), { STATIC_MCP_BEARER_SECRET: "prod-static" }),
    { ok: false, code: "mcp_auth_failed" },
  );
});

test("production issuer RS256 JWT validates with migrated public PEM", async () => {
  const kp = await keyPair();
  const pem = await publicPem(kp.publicKey);
  const now = Math.floor(Date.now() / 1000);
  const token = await jwt(kp.privateKey, {
    iss: ISSUER,
    aud: `${ISSUER}/mcp`,
    sub: "https://chatgpt.com/client",
    client_id: "https://chatgpt.com/client",
    iat: now,
    exp: now + 3600,
  });
  const result = await authenticateMcpRequest(request(token), {
    OAUTH_ISSUER: ISSUER,
    OAUTH_JWT_PUBLIC_PEM: pem,
  });
  assert.equal(result.ok, true);
  assert.equal(result.source, "oauth_jwt");
  assert.equal(result.clientId, "https://chatgpt.com/client");
});

test("wrong audience, issuer, signature, or missing OAuth config fail closed", async () => {
  const kp = await keyPair();
  const other = await keyPair();
  const pem = await publicPem(kp.publicKey);
  const now = Math.floor(Date.now() / 1000);
  for (const claims of [
    { iss: ISSUER, aud: "https://other.example/mcp", exp: now + 3600 },
    { iss: "https://other.example", aud: `${ISSUER}/mcp`, exp: now + 3600 },
    { iss: ISSUER, exp: now + 3600 },
  ]) {
    const token = await jwt(kp.privateKey, claims);
    assert.deepEqual(
      await authenticateMcpRequest(request(token), { OAUTH_ISSUER: ISSUER, OAUTH_JWT_PUBLIC_PEM: pem }),
      { ok: false, code: "mcp_auth_failed" },
    );
  }
  const badSig = await jwt(other.privateKey, { iss: ISSUER, aud: `${ISSUER}/mcp`, exp: now + 3600 });
  assert.deepEqual(
    await authenticateMcpRequest(request(badSig), { OAUTH_ISSUER: ISSUER, OAUTH_JWT_PUBLIC_PEM: pem }),
    { ok: false, code: "mcp_auth_failed" },
  );
  const good = await jwt(kp.privateKey, { iss: ISSUER, aud: `${ISSUER}/mcp`, exp: now + 3600 });
  assert.deepEqual(await authenticateMcpRequest(request(good), {}), { ok: false, code: "mcp_auth_failed" });
});

test("public key PEM importer rejects malformed input", async () => {
  await assert.rejects(() => importRs256PublicKeyPem("not a pem"), /invalid PUBLIC KEY PEM/);
});
