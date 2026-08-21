#!/usr/bin/env node
/**
 * OAuth 2.1 upgrade acceptance tests (ChatGPT/OpenAI OAuth compatibility).
 *
 * Spawns dist/server.js on a test port with:
 *   HERDR_MCP_TOKEN=test-token          (static Bearer compat)
 *   HERDR_MCP_OAUTH_DIR=<tmp>           (isolated persistent registry)
 *
 * Covers (acceptance list):
 *   - every discovery URL returns 200 with absolute endpoint URLs
 *   - /.well-known/openid-configuration no longer 404 (RFC 8414 §5 OAuth doc;
 *     no openid/userinfo/id_token claims advertised)
 *   - 401 /mcp carries WWW-Authenticate: Bearer resource_metadata=…
 *   - full local DCR + PKCE S256 flow (register → authorize → token → /mcp)
 *   - wrong PKCE verifier rejected
 *   - authorization code reuse rejected (one-use)
 *   - refresh rotation (new access+refresh) and old refresh reuse rejected
 *   - persistence-reload: restart with the same OAUTH_DIR — stored clients,
 *     access tokens and refresh tokens survive
 *   - static HERDR_MCP_TOKEN still authenticates /mcp (Claude compat)
 *
 * Usage: node tests/oauth_flow.mjs
 */
import { spawn } from "node:child_process";
import { once } from "node:events";
import { createHash, randomBytes } from "node:crypto";
import { mkdtempSync, rmSync, readdirSync, statSync } from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const PORT = 8801;
const TOKEN = "test-token";
const BASE = `http://127.0.0.1:${PORT}`;
const RESOURCE = `${BASE}/mcp`; // canonical protected resource = issuer + /mcp (RFC 9728)
const OAUTH_DIR = mkdtempSync(path.join(os.tmpdir(), "herdr-mcp-oauth-test-"));

let failures = 0;
let server = null;

function ok(cond, label, detail = "") {
  if (cond) console.log(`  ✅ ${label}`);
  else { failures++; console.error(`  ❌ ${label}${detail ? ` — ${detail}` : ""}`); }
}

// ---------------------------------------------------------------------------
// Server lifecycle
// ---------------------------------------------------------------------------
async function startServer() {
  server = spawn("node", [path.join(__dirname, "..", "dist", "server.js")], {
    env: {
      ...process.env,
      HERDR_MCP_PORT: String(PORT),
      HERDR_MCP_TOKEN: TOKEN,
      HERDR_MCP_OAUTH_DIR: OAUTH_DIR,
    },
    stdio: ["ignore", "pipe", "pipe"],
  });
  let out = "";
  server.stdout.on("data", (d) => { out += d; });
  server.stderr.on("data", (d) => { out += d; });
  const deadline = Date.now() + 8000;
  while (Date.now() < deadline) {
    if (out.includes("listening on")) return;
    if (server.exitCode !== null) throw new Error(`server exited ${server.exitCode}: ${out}`);
    await new Promise((r) => setTimeout(r, 100));
  }
  throw new Error("server did not start in time");
}

async function stopServer() {
  if (!server) return;
  server.kill("SIGTERM");
  await once(server, "exit").catch(() => {});
  server = null;
}

// ---------------------------------------------------------------------------
// HTTP helpers
// ---------------------------------------------------------------------------
async function getJson(url) {
  const r = await fetch(url);
  const body = await r.text();
  let json = null;
  try { json = JSON.parse(body); } catch { /* non-JSON */ }
  return { status: r.status, json, headers: r.headers };
}

async function postJson(url, payload, token) {
  const r = await fetch(url, {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      ...(token ? { Authorization: `Bearer ${token}` } : {}),
    },
    body: JSON.stringify(payload),
  });
  const body = await r.text();
  let json = null;
  try { json = JSON.parse(body); } catch { /* non-JSON */ }
  return { status: r.status, json, headers: r.headers };
}

async function postForm(url, params, token) {
  const r = await fetch(url, {
    method: "POST",
    headers: {
      "Content-Type": "application/x-www-form-urlencoded",
      ...(token ? { Authorization: `Bearer ${token}` } : {}),
    },
    body: new URLSearchParams(params).toString(),
  });
  const body = await r.text();
  let json = null;
  try { json = JSON.parse(body); } catch { /* non-JSON */ }
  return { status: r.status, json, headers: r.headers };
}

async function mcpPost(token, method = "initialize") {
  const r = await fetch(`${BASE}/mcp`, {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      Accept: "application/json, text/event-stream",
      ...(token ? { Authorization: `Bearer ${token}` } : {}),
    },
    body: JSON.stringify({
      jsonrpc: "2.0", id: 1, method,
      params: { protocolVersion: "2025-11-25", capabilities: {}, clientInfo: { name: "oauth-test", version: "1" } },
    }),
  });
  const body = await r.text();
  return { status: r.status, headers: r.headers, body };
}

// ---------------------------------------------------------------------------
// PKCE helpers (RFC 7636 S256)
// ---------------------------------------------------------------------------
function pkce() {
  const verifier = randomBytes(48).toString("base64url"); // 64 chars, safe charset
  const challenge = createHash("sha256").update(verifier).digest("base64url");
  return { verifier, challenge };
}

function isAbsoluteUrl(u) {
  try {
    const p = new URL(u);
    return p.protocol === "http:" || p.protocol === "https:";
  } catch { return false; }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
async function testDiscovery() {
  console.log("\n1) Discovery metadata (RFC 8414 / RFC 9728 / OIDC-compat)");
  const as = await getJson(`${BASE}/.well-known/oauth-authorization-server`);
  ok(as.status === 200, "oauth-authorization-server 200", `status=${as.status}`);
  ok(as.json.issuer === BASE, "issuer === base URL", `got ${as.json.issuer}`);
  for (const k of ["authorization_endpoint", "token_endpoint", "registration_endpoint"]) {
    ok(isAbsoluteUrl(as.json[k]), `${k} absolute`, `${k}=${as.json[k]}`);
  }
  ok(Array.isArray(as.json.scopes_supported) && as.json.scopes_supported.includes("mcp"),
    "scopes_supported includes mcp");
  ok(!as.json.scopes_supported.includes("openid"), "no openid scope advertised");
  ok(Array.isArray(as.json.code_challenge_methods_supported) && as.json.code_challenge_methods_supported.includes("S256"),
    "code_challenge_methods_supported includes S256");
  ok(as.json.grant_types_supported.includes("refresh_token"), "refresh_token grant advertised");
  ok(as.json.response_types_supported.includes("code"), "response_types_supported includes code");

  const oidc = await getJson(`${BASE}/.well-known/openid-configuration`);
  ok(oidc.status === 200, "openid-configuration 200 (was 404 → ChatGPT stop)", `status=${oidc.status}`);
  ok(oidc.json.issuer === BASE, "openid-configuration issuer === base URL");
  ok(isAbsoluteUrl(oidc.json.authorization_endpoint) && isAbsoluteUrl(oidc.json.token_endpoint),
    "openid-configuration endpoints absolute");
  ok(!("userinfo_endpoint" in oidc.json), "no userinfo_endpoint (no fake OIDC)");
  ok(!("id_token_signing_alg_values_supported" in oidc.json), "no id_token alg claim (no fake OIDC)");
  ok(!(oidc.json.scopes_supported ?? []).includes("openid"), "no openid scope in oidc doc");
  ok(oidc.json.authorization_endpoint === as.json.authorization_endpoint,
    "openid doc endpoints match oauth-authorization-server doc");

  const prRoot = await getJson(`${BASE}/.well-known/oauth-protected-resource`);
  ok(prRoot.status === 200 && prRoot.json.resource === BASE,
    "oauth-protected-resource (root) 200, resource === base", `got ${prRoot.json.resource}`);
  ok(prRoot.json.authorization_servers?.[0] === BASE, "authorization_servers[0] === base");

  const prMcp = await getJson(`${BASE}/.well-known/oauth-protected-resource/mcp`);
  ok(prMcp.status === 200 && prMcp.json.resource === RESOURCE,
    "oauth-protected-resource/mcp 200, resource === canonical /mcp", `got ${prMcp.json.resource}`);
  ok(prMcp.json.authorization_servers?.[0] === BASE, "path-aware authorization_servers[0] === base");

  const card = await getJson(`${BASE}/.well-known/mcp.json`);
  ok(card.status === 200 && card.json.serverUrl === RESOURCE,
    "mcp.json 200 with absolute serverUrl === /mcp", `got ${card.json.serverUrl}`);
  ok(card.json.version === "0.3.4",
    "mcp.json version must be 0.3.4 (cache invalidation identity)", `got ${card.json.version}`);

  // Path-aware AS + CORS (ChatGPT browser discovery) + CIMD flag + /mcp/register
  const asMcp = await getJson(`${BASE}/.well-known/oauth-authorization-server/mcp`);
  ok(asMcp.status === 200, "AS metadata /mcp path-aware 200", `status=${asMcp.status}`);
  ok(asMcp.json.client_id_metadata_document_supported === true,
    "AS advertises client_id_metadata_document_supported (CIMD)", JSON.stringify(asMcp.json.client_id_metadata_document_supported));
  const cors = await fetch(`${BASE}/.well-known/oauth-authorization-server`, {
    headers: { Origin: "https://chatgpt.com", Accept: "application/json" },
  });
  ok(cors.headers.get("access-control-allow-origin") === "*",
    "AS metadata CORS Allow-Origin *", `got ${cors.headers.get("access-control-allow-origin")}`);
  const opt = await fetch(`${BASE}/mcp`, {
    method: "OPTIONS",
    headers: {
      Origin: "https://chatgpt.com",
      "Access-Control-Request-Method": "POST",
      "Access-Control-Request-Headers": "authorization,content-type",
    },
  });
  ok(opt.status === 204 || opt.status === 200, "OPTIONS /mcp not 401", `status=${opt.status}`);
  ok(opt.headers.get("access-control-allow-origin") === "*",
    "OPTIONS /mcp CORS", `got ${opt.headers.get("access-control-allow-origin")}`);
  const mcpReg = await postJson(`${BASE}/mcp/register`, {
    client_name: "path-dcr",
    redirect_uris: ["https://chatgpt.com/connector/oauth/callback"],
    token_endpoint_auth_method: "none",
    grant_types: ["authorization_code", "refresh_token"],
    response_types: ["code"],
  });
  ok(mcpReg.status === 201, "POST /mcp/register → 201", `status=${mcpReg.status}`);

  console.log("\n1b) Path-aware AS discovery (ChatGPT /mcp server URL, RFC 8414 §4)");
  const pathAware = [
    `${BASE}/.well-known/oauth-authorization-server/mcp`,
    `${BASE}/.well-known/openid-configuration/mcp`,
    `${BASE}/mcp/.well-known/oauth-authorization-server`,
    `${BASE}/mcp/.well-known/openid-configuration`,
  ];
  for (const p of pathAware) {
    const r = await getJson(p);
    ok(r.status === 200, `${p} 200 (must NOT 401)`, `status=${r.status}`);
    ok(r.json.issuer === BASE, `${p} issuer === base`, `got ${r.json.issuer}`);
    ok(r.json.registration_endpoint === as.json.registration_endpoint,
      `${p} registration_endpoint matches root doc`, r.json.registration_endpoint);
  }
  const prMcpPath = await getJson(`${BASE}/mcp/.well-known/oauth-protected-resource`);
  ok(prMcpPath.status === 200 && prMcpPath.json.resource === RESOURCE,
    "/mcp/.well-known/oauth-protected-resource 200, resource === canonical /mcp", `got ${prMcpPath.json.resource}`);
}

async function test401Challenge() {
  console.log("\n2) 401 /mcp with WWW-Authenticate resource_metadata");
  const r = await mcpPost(null);
  ok(r.status === 401, "unauthenticated /mcp → 401", `status=${r.status}`);
  const wa = r.headers.get("www-authenticate") ?? "";
  ok(wa.includes("Bearer"), "WWW-Authenticate Bearer scheme", wa);
  ok(wa.includes(`resource_metadata="${BASE}/.well-known/oauth-protected-resource/mcp"`),
    "WWW-Authenticate resource_metadata points at path-aware protected-resource (/mcp)", wa);
}

async function testDcrRoutes() {
  console.log("\n2b) DCR register routes: canonical + trailing slash + ChatGPT /register fallback");
  const payload = {
    redirect_uris: [`${BASE}/callback`],
    client_name: "dcr-route-test",
    grant_types: ["authorization_code", "refresh_token"],
    token_endpoint_auth_method: "none",
  };
  // canonical
  const canonical = await postJson(`${BASE}/oauth/register`, payload);
  ok(canonical.status === 201, "POST /oauth/register → 201", `status=${canonical.status}`);
  ok(typeof canonical.json.client_id === "string", "canonical returns client_id");
  // trailing slash
  const trailing = await postJson(`${BASE}/oauth/register/`, payload);
  ok(trailing.status === 201, "POST /oauth/register/ → 201", `status=${trailing.status}`);
  ok(typeof trailing.json.client_id === "string", "trailing-slash returns client_id");
  // ChatGPT fallback /register
  const fallback = await postJson(`${BASE}/register`, payload);
  ok(fallback.status === 201, "POST /register → 201 (ChatGPT DCR fallback)", `status=${fallback.status}`);
  ok(typeof fallback.json.client_id === "string", "/register returns client_id");
  // fallback trailing slash
  const fallbackSlash = await postJson(`${BASE}/register/`, payload);
  ok(fallbackSlash.status === 201, "POST /register/ → 201", `status=${fallbackSlash.status}`);
  ok(typeof fallbackSlash.json.client_id === "string", "/register/ returns client_id");
  // ChatGPT may resolve DCR relative to the /mcp connector URL.
  const mcpOauthReg = await postJson(`${BASE}/mcp/oauth/register`, payload);
  ok(mcpOauthReg.status === 201, "POST /mcp/oauth/register → 201", `status=${mcpOauthReg.status}`);
  // /.well-known/oauth-registration is not a route either
  const wkReg = await postJson(`${BASE}/.well-known/oauth-registration`, payload);
  ok(wkReg.status === 401 || wkReg.status === 404,
    "POST /.well-known/oauth-registration → 401/404 (not a route)", `status=${wkReg.status}`);
}

async function fullDcrPkceFlow() {
  console.log("\n3) Full DCR + PKCE S256 flow");
  const registered = await postJson(`${BASE}/oauth/register`, {
    redirect_uris: [`${BASE}/callback`],
    client_name: "oauth-test-client",
    grant_types: ["authorization_code", "refresh_token"],
    token_endpoint_auth_method: "none",
  });
  ok(registered.status === 201, "DCR register → 201", `status=${registered.status}`);
  const clientId = registered.json.client_id;
  ok(typeof clientId === "string" && clientId.length > 0, "client_id returned");
  ok(typeof registered.json.client_secret === "string", "client_secret returned");
  ok(registered.json.grant_types.includes("refresh_token"), "registered grant_types keep refresh_token");

  const p = pkce();
  const au = await fetch(
    `${BASE}/oauth/authorize?response_type=code&client_id=${encodeURIComponent(clientId)}` +
    `&redirect_uri=${encodeURIComponent(`${BASE}/callback`)}&state=xyz123` +
    `&code_challenge=${p.challenge}&code_challenge_method=S256&resource=${encodeURIComponent(RESOURCE)}`,
    { redirect: "manual" },
  );
  ok(au.status === 302, "authorize → 302 redirect", `status=${au.status}`);
  const loc = au.headers.get("location") ?? "";
  ok(loc.includes("code="), "Location carries code");
  ok(loc.includes("state=xyz123"), "Location echoes state");
  ok(loc.includes(`iss=${encodeURIComponent(BASE)}`), "Location carries RFC 9207 iss", loc);
  const code = new URL(loc).searchParams.get("code");

  const tok = await postForm(`${BASE}/oauth/token`, {
    grant_type: "authorization_code",
    code,
    redirect_uri: `${BASE}/callback`,
    client_id: clientId,
    code_verifier: p.verifier,
    resource: RESOURCE,
  });
  ok(tok.status === 200, "token exchange → 200", `status=${tok.status} ${JSON.stringify(tok.json)}`);
  const accessToken = tok.json.access_token;
  ok(typeof accessToken === "string" && accessToken.length > 0, "opaque access_token returned");
  ok(accessToken !== TOKEN, "access_token is NOT the static HERDR_MCP_TOKEN");
  ok(tok.json.token_type === "Bearer", "token_type Bearer", tok.json.token_type);
  ok(typeof tok.json.refresh_token === "string", "refresh_token returned");
  ok(tok.json.scope === "mcp", "scope mcp returned", tok.json.scope);
  ok(tok.json.expires_in > 0, "expires_in positive");

  const mcp = await mcpPost(accessToken);
  ok(mcp.status === 200, "/mcp accepts OAuth access token", `status=${mcp.status}`);

  return { clientId, accessToken, refreshToken: tok.json.refresh_token };
}

async function testWrongVerifier(clientId) {
  console.log("\n4) Wrong PKCE verifier rejected");
  const p = pkce();
  const p2 = pkce();
  const au = await fetch(
    `${BASE}/oauth/authorize?response_type=code&client_id=${encodeURIComponent(clientId)}` +
    `&redirect_uri=${encodeURIComponent(`${BASE}/callback`)}` +
    `&code_challenge=${p.challenge}&code_challenge_method=S256`,
    { redirect: "manual" },
  );
  const code = new URL(au.headers.get("location")).searchParams.get("code");
  const tok = await postForm(`${BASE}/oauth/token`, {
    grant_type: "authorization_code",
    code,
    redirect_uri: `${BASE}/callback`,
    client_id: clientId,
    code_verifier: p2.verifier, // wrong verifier
  });
  ok(tok.status === 400 && tok.json.error === "invalid_grant",
    "wrong verifier → 400 invalid_grant", `status=${tok.status} ${JSON.stringify(tok.json)}`);
}

async function testCodeReuse(clientId, verifier) {
  console.log("\n5) Authorization code reuse rejected (one-use)");
  const p = pkce();
  const au = await fetch(
    `${BASE}/oauth/authorize?response_type=code&client_id=${encodeURIComponent(clientId)}` +
    `&redirect_uri=${encodeURIComponent(`${BASE}/callback`)}` +
    `&code_challenge=${p.challenge}&code_challenge_method=S256`,
    { redirect: "manual" },
  );
  const code = new URL(au.headers.get("location")).searchParams.get("code");
  const body = {
    grant_type: "authorization_code",
    code,
    redirect_uri: `${BASE}/callback`,
    client_id: clientId,
    code_verifier: p.verifier,
  };
  const first = await postForm(`${BASE}/oauth/token`, body);
  const second = await postForm(`${BASE}/oauth/token`, body);
  ok(first.status === 200, "first exchange succeeds");
  ok(second.status === 400 && second.json.error === "invalid_grant",
    "second exchange with same code → 400 invalid_grant", `status=${second.status} ${JSON.stringify(second.json)}`);
  void verifier;
}

async function testRefreshRotation(clientId, refreshToken) {
  console.log("\n6) Refresh token rotation + reuse rejection");
  const refreshed = await postForm(`${BASE}/oauth/token`, {
    grant_type: "refresh_token",
    refresh_token: refreshToken,
    client_id: clientId,
  });
  ok(refreshed.status === 200, "refresh → 200", `status=${refreshed.status} ${JSON.stringify(refreshed.json)}`);
  ok(typeof refreshed.json.access_token === "string", "new access_token issued");
  const newRefresh = refreshed.json.refresh_token;
  ok(typeof newRefresh === "string" && newRefresh !== refreshToken, "new refresh_token issued (rotation)");

  const reuse = await postForm(`${BASE}/oauth/token`, {
    grant_type: "refresh_token",
    refresh_token: refreshToken, // old one
    client_id: clientId,
  });
  ok(reuse.status === 400 && reuse.json.error === "invalid_grant",
    "old refresh_token reuse → 400 invalid_grant", `status=${reuse.status} ${JSON.stringify(reuse.json)}`);

  return { accessToken: refreshed.json.access_token, refreshToken: newRefresh };
}

async function testPersistenceReload(clientId, accessToken, refreshToken) {
  console.log("\n7) Persistence-reload (restart with same OAUTH_DIR)");
  // Files exist and are 0600.
  const files = ["clients.json", "tokens.json", "refresh.json"];
  const present = files.every((f) => {
    try { return statSync(path.join(OAUTH_DIR, f)).isFile(); } catch { return false; }
  });
  ok(present, "clients.json / tokens.json / refresh.json persisted", readdirSync(OAUTH_DIR).join(","));
  ok(files.every((f) => (statSync(path.join(OAUTH_DIR, f)).mode & 0o777) === 0o600),
    "persisted files are mode 0600");

  await stopServer();
  await startServer();

  const mcp = await mcpPost(accessToken);
  ok(mcp.status === 200, "pre-restart access_token still valid after reload", `status=${mcp.status}`);

  const refreshed = await postForm(`${BASE}/oauth/token`, {
    grant_type: "refresh_token",
    refresh_token: refreshToken,
    client_id: clientId,
  });
  ok(refreshed.status === 200, "refresh_token survives restart (registry reloaded)",
    `status=${refreshed.status} ${JSON.stringify(refreshed.json)}`);
  return refreshed.json.access_token;
}

async function testStaticToken() {
  console.log("\n8) Static HERDR_MCP_TOKEN still authenticates /mcp (Claude compat)");
  const mcp = await mcpPost(TOKEN);
  ok(mcp.status === 200, "static token → 200", `status=${mcp.status}`);
  const bad = await mcpPost("not-the-token");
  ok(bad.status === 401, "wrong static token → 401", `status=${bad.status}`);
}

async function testAuthorizeValidation() {
  console.log("\n9) Authorize endpoint validation");
  const noClient = await fetch(
    `${BASE}/oauth/authorize?response_type=code&client_id=dcr-nonexistent` +
    `&redirect_uri=${encodeURIComponent(`${BASE}/callback`)}&code_challenge=abc&code_challenge_method=S256`,
    { redirect: "manual" },
  );
  ok(noClient.status === 400, "unknown client_id → 400 (no redirect)", `status=${noClient.status}`);

  const registered = await postJson(`${BASE}/oauth/register`, {
    redirect_uris: [`${BASE}/callback`],
    client_name: "val-test",
    grant_types: ["authorization_code", "refresh_token"],
    token_endpoint_auth_method: "none",
  });
  const wrongRedirect = await fetch(
    `${BASE}/oauth/authorize?response_type=code&client_id=${encodeURIComponent(registered.json.client_id)}` +
    `&redirect_uri=${encodeURIComponent(`${BASE}/evil-callback`)}` +
    `&code_challenge=abc&code_challenge_method=S256`,
    { redirect: "manual" },
  );
  ok(wrongRedirect.status === 400, "unregistered redirect_uri → 400 (no redirect)", `status=${wrongRedirect.status}`);

  const noPkce = await fetch(
    `${BASE}/oauth/authorize?response_type=code&client_id=${encodeURIComponent(registered.json.client_id)}` +
    `&redirect_uri=${encodeURIComponent(`${BASE}/callback`)}`,
    { redirect: "manual" },
  );
  ok(noPkce.status === 302 && (noPkce.headers.get("location") ?? "").includes("error=invalid_request"),
    "missing code_challenge → error redirect", `status=${noPkce.status} ${noPkce.headers.get("location")}`);
}

// ---------------------------------------------------------------------------
async function main() {
  try {
    await startServer();
    console.log(`server on ${BASE}, oauth dir ${OAUTH_DIR}`);

    await testDiscovery();
    await test401Challenge();
    await testDcrRoutes();
    const { clientId, accessToken, refreshToken } = await fullDcrPkceFlow();
    await testWrongVerifier(clientId);
    await testCodeReuse(clientId);
    const rotated = await testRefreshRotation(clientId, refreshToken);
    await testStaticToken();
    await testAuthorizeValidation();
    await testPersistenceReload(clientId, accessToken, rotated.refreshToken);

    console.log(`\n${failures === 0 ? "✅ ALL PASS" : `❌ ${failures} FAILURE(S)`}`);
  } finally {
    await stopServer();
    rmSync(OAUTH_DIR, { recursive: true, force: true });
  }
  process.exit(failures === 0 ? 0 : 1);
}

main().catch((e) => { console.error(e); process.exit(1); });
