/**
 * herdr-mcp — OAuth 2.1 authorization server (DCR + authorization-code + PKCE).
 *
 * Replaces the previous hand-written OAuth (static AUTH_TOKEN reused as the
 * OAuth access token, in-memory maps, no PKCE verification, no discovery
 * compatibility) with a focused, spec-compliant implementation:
 *
 *  - RFC 8414 authorization-server metadata + RFC 9728 protected-resource
 *    metadata, all endpoint URLs absolute, `scopes_supported` advertised.
 *  - `/.well-known/openid-configuration` compatibility endpoint (RFC 8414
 *    §5: an OAuth-only AS may publish its OAuth 2.0 metadata at the OIDC
 *    discovery URL). NO OIDC claims are advertised — no `openid` scope, no
 *    userinfo_endpoint, no id_token_signing_alg_values_supported — this is
 *    OAuth 2.0 metadata, not a claim of OpenID Connect support. ChatGPT's
 *    client probes this URL after RFC 8414 discovery; a 404 previously made
 *    it stop with "does not implement OAuth".
 *  - RFC 9207 `iss` returned on the authorize callback (and error redirects).
 *  - 401 /mcp challenges carry `WWW-Authenticate: Bearer resource_metadata=…`
 *    (RFC 9728 §5.1) so clients discover OAuth from the resource server.
 *  - Dynamic client registry (RFC 7591) + JWT access tokens (aud=resource) and
 *    opaque rotating refresh tokens, persisted under ~/.config/herdr-mcp/oauth
 *    (override HERDR_MCP_OAUTH_DIR) — restart-safe. JWT access tokens follow
 *    OpenAI's guidance to embed the resource as `aud`
 *    (https://developers.openai.com/plugins/build/auth.md).
 *  - Authorization codes are one-use and bound to client_id / redirect_uri /
 *    resource; PKCE S256 code_verifier is required and verified.
 *  - Refresh tokens rotate: each refresh mints a new access+refresh token and
 *    the previous refresh token is rejected on reuse.
 *  - `resource` is normalized (missing / base URL / base+"/mcp" all map to the
 *    canonical protected resource) and cross-checked at the token endpoint.
 *  - CIMD: `client_id_metadata_document_supported` + ChatGPT HTTPS client_ids;
 *    token endpoint accepts `none` and verifies `private_key_jwt` against the
 *    CIMD JWKS when a client_assertion is presented.
 *
 * Claude compatibility is retained: HERDR_MCP_TOKEN still validates as a
 * static Bearer credential on /mcp, and the DCR + PKCE + refresh flow is
 * exactly what Claude's connector already performs.
 */
import { randomBytes, createHash, timingSafeEqual, generateKeyPairSync } from "node:crypto";
import { readFileSync, existsSync } from "node:fs";
import { writeFile, mkdir, chmod, rename } from "node:fs/promises";
import * as path from "node:path";
import * as os from "node:os";
import { Router, Request, Response, NextFunction } from "express";
import type { Express } from "express";
import {
  SignJWT,
  jwtVerify,
  importPKCS8,
  importSPKI,
  createRemoteJWKSet,
  type CryptoKey,
} from "jose";
import { SERVER_NAME, SERVER_VERSION } from "./version.js";
import { extensionSessionBearerMatches } from "./extension-auth.js";

// ---------------------------------------------------------------------------
// Config (mirrors server.ts env handling; self-contained on purpose)
// ---------------------------------------------------------------------------
const PORT = Number(process.env.HERDR_MCP_PORT ?? "8772");
const BASE_URL = process.env.HERDR_MCP_BASE_URL ?? "";
const AUTH_TOKEN = process.env.HERDR_MCP_TOKEN ?? "";
const OAUTH_DIR =
  process.env.HERDR_MCP_OAUTH_DIR ??
  path.join(os.homedir(), ".config", "herdr-mcp", "oauth");

const SCOPE = "mcp";
const ACCESS_TOKEN_TTL_S = Number(process.env.HERDR_MCP_OAUTH_ACCESS_TTL_S ?? "86400");
const REFRESH_TOKEN_TTL_S = Number(process.env.HERDR_MCP_OAUTH_REFRESH_TTL_S ?? "2592000"); // 30 days
const CODE_TTL_MS = 5 * 60_000;
const PKCE_VERIFIER_RE = /^[A-Za-z0-9\-._~]{43,128}$/;

// RS256 keypair for access-token JWTs (aud=resource). Generated once under OAUTH_DIR.
let jwtPrivateKey: CryptoKey | null = null;
let jwtPublicKey: CryptoKey | null = null;
const jwtReady: Promise<void> = (async () => {
  await mkdir(OAUTH_DIR, { recursive: true });
  const privPath = path.join(OAUTH_DIR, "jwt-private.pem");
  const pubPath = path.join(OAUTH_DIR, "jwt-public.pem");
  try {
    if (existsSync(privPath) && existsSync(pubPath)) {
      jwtPrivateKey = await importPKCS8(readFileSync(privPath, "utf8"), "RS256");
      jwtPublicKey = await importSPKI(readFileSync(pubPath, "utf8"), "RS256");
      return;
    }
  } catch (e) {
    console.error("[herdr-mcp] oauth jwt key load failed, regenerating:", e);
  }
  const { privateKey, publicKey } = generateKeyPairSync("rsa", {
    modulusLength: 2048,
    publicKeyEncoding: { type: "spki", format: "pem" },
    privateKeyEncoding: { type: "pkcs8", format: "pem" },
  });
  await writeFile(privPath, privateKey, { mode: 0o600 });
  await writeFile(pubPath, publicKey, { mode: 0o644 });
  try { await chmod(privPath, 0o600); } catch { /* best-effort */ }
  jwtPrivateKey = await importPKCS8(privateKey, "RS256");
  jwtPublicKey = await importSPKI(publicKey, "RS256");
})();

/** The OAuth issuer identifier (RFC 8414) — the base URL of this server. */
export function oauthIssuer(): string {
  return (BASE_URL || `http://127.0.0.1:${PORT}`).replace(/\/+$/, "");
}

/**
 * Canonical protected resource identifier. The MCP endpoint is served at
 * `<issuer>/mcp` (the configured server URL), so per RFC 9728 the protected
 * resource is `<issuer>/mcp`, not the bare issuer. ChatGPT/ctmc may still
 * supply the issuer base in an authorization request; normalizeResource maps
 * both the issuer and `<issuer>/mcp` to this canonical resource so the token
 * audience, 401 challenge, and authorization request stay consistent.
 */
export function oauthResourceUrl(): string {
  return `${oauthIssuer()}/mcp`;
}

/** RFC 9728 §5.1: path-aware protected-resource metadata for the /mcp resource. */
function protectedResourceMetadataUrl(): string {
  return `${oauthIssuer()}/.well-known/oauth-protected-resource/mcp`;
}

// ---------------------------------------------------------------------------
// Crypto helpers
// ---------------------------------------------------------------------------
function sha256Hex(s: string): string {
  return createHash("sha256").update(s, "utf8").digest("hex");
}

/** Hash an opaque token before storing it (store never holds plaintext tokens). */
function hashToken(t: string): string {
  return sha256Hex(`herdr-mcp-oauth:${t}`);
}

function newToken(): string {
  return randomBytes(32).toString("base64url");
}

function safeEqual(a: string, b: string): boolean {
  const ba = Buffer.from(a, "utf8");
  const bb = Buffer.from(b, "utf8");
  if (ba.length !== bb.length) return false;
  return timingSafeEqual(ba, bb);
}

/** RFC 7636 §4.6: S256 = BASE64URL(SHA256(ASCII(code_verifier))). */
function verifyPkce(codeVerifier: string, codeChallenge: string): boolean {
  if (!PKCE_VERIFIER_RE.test(codeVerifier)) return false;
  const computed = createHash("sha256").update(codeVerifier, "utf8").digest("base64url");
  return safeEqual(computed, codeChallenge);
}

/**
 * Resource normalization (ctmc lesson): clients pass `resource` in many
 * shapes — missing/empty, the bare issuer, or the canonical resource URL.
 * All map to the canonical protected resource; anything foreign is rejected
 * (the resource must be checked, not silently accepted).
 */
function normalizeResource(r: string): string | null {
  const iss = oauthIssuer();
  const canonical = oauthResourceUrl();
  if (r === "") return canonical; // omitted → default resource
  const trimmed = r.replace(/\/+$/, "");
  if (trimmed === iss || trimmed === `${iss}/mcp`) return canonical;
  return null;
}

// ---------------------------------------------------------------------------
// Persistent store: clients / access tokens / refresh tokens
// ---------------------------------------------------------------------------
interface StoredClient {
  client_secret_hash: string | null;
  redirect_uris: string[];
  token_endpoint_auth_method: "none" | "client_secret_post";
  grant_types: string[];
  scope: string;
  client_name?: string;
  issued_at: number;
}
interface AccessTokenRecord {
  client_id: string;
  resource: string;
  scope: string;
  expires_at: number; // epoch seconds
}
type RefreshTokenRecord = AccessTokenRecord;
interface AuthCodeRecord {
  client_id: string;
  redirect_uri: string;
  code_challenge: string;
  resource: string;
  expires_at: number; // epoch ms
}

const clients = new Map<string, StoredClient>();
const accessTokens = new Map<string, AccessTokenRecord>();
const refreshTokens = new Map<string, RefreshTokenRecord>();
// Codes are short-lived (5 min) and in-memory only — same as the ctmc patch.
const codes = new Map<string, AuthCodeRecord>();

function readJsonSync(kind: string): Record<string, unknown> {
  try {
    const raw = readFileSync(path.join(OAUTH_DIR, `${kind}.json`), "utf8");
    const parsed = JSON.parse(raw) as unknown;
    return parsed !== null && typeof parsed === "object" ? (parsed as Record<string, unknown>) : {};
  } catch {
    return {}; // missing/corrupt → empty (corrupt never blocks startup)
  }
}

function loadAll(): void {
  const now = Math.floor(Date.now() / 1000);
  for (const [id, v] of Object.entries(readJsonSync("clients"))) {
    const c = (v ?? {}) as Partial<StoredClient>;
    if (!id || !Array.isArray(c.redirect_uris)) continue;
    clients.set(id, {
      client_secret_hash: typeof c.client_secret_hash === "string" ? c.client_secret_hash : null,
      redirect_uris: c.redirect_uris.filter((u): u is string => typeof u === "string"),
      token_endpoint_auth_method: c.token_endpoint_auth_method === "client_secret_post" ? "client_secret_post" : "none",
      grant_types: Array.isArray(c.grant_types)
        ? c.grant_types.filter((g): g is string => typeof g === "string")
        : ["authorization_code", "refresh_token"],
      scope: typeof c.scope === "string" ? c.scope : SCOPE,
      ...(typeof c.client_name === "string" ? { client_name: c.client_name } : {}),
      issued_at: typeof c.issued_at === "number" ? c.issued_at : now,
    });
  }
  for (const [h, v] of Object.entries(readJsonSync("tokens"))) {
    const t = (v ?? {}) as Partial<AccessTokenRecord>;
    if (!h || typeof t.client_id !== "string" || typeof t.expires_at !== "number" || t.expires_at <= now) continue;
    accessTokens.set(h, {
      client_id: t.client_id,
      resource: typeof t.resource === "string" ? t.resource : oauthResourceUrl(),
      scope: typeof t.scope === "string" ? t.scope : SCOPE,
      expires_at: t.expires_at,
    });
  }
  for (const [h, v] of Object.entries(readJsonSync("refresh"))) {
    const t = (v ?? {}) as Partial<RefreshTokenRecord>;
    if (!h || typeof t.client_id !== "string" || typeof t.expires_at !== "number" || t.expires_at <= now) continue;
    refreshTokens.set(h, {
      client_id: t.client_id,
      resource: typeof t.resource === "string" ? t.resource : oauthResourceUrl(),
      scope: typeof t.scope === "string" ? t.scope : SCOPE,
      expires_at: t.expires_at,
    });
  }
}

/** Atomic persist (tmp + chmod 0600 + rename). Best-effort, like ctmc's patch. */
async function persist(kind: "clients" | "tokens" | "refresh"): Promise<void> {
  const data =
    kind === "clients"
      ? Object.fromEntries(clients)
      : kind === "tokens"
        ? Object.fromEntries(accessTokens)
        : Object.fromEntries(refreshTokens);
  try {
    await mkdir(OAUTH_DIR, { recursive: true });
    const p = path.join(OAUTH_DIR, `${kind}.json`);
    const tmp = `${p}.${process.pid}.${randomBytes(4).toString("hex")}.tmp`;
    await writeFile(tmp, JSON.stringify(data, null, 2), { encoding: "utf8", mode: 0o600 });
    await chmod(tmp, 0o600);
    await rename(tmp, p);
  } catch (e) {
    console.error(`[herdr-mcp] oauth persist ${kind} failed:`, e);
  }
}

loadAll();

// ---------------------------------------------------------------------------
// Metadata documents
// ---------------------------------------------------------------------------
/**
 * RFC 8414 §2 authorization-server metadata. The exact same document is
 * served at `/.well-known/openid-configuration` (RFC 8414 §5 — a legitimate
 * OAuth-only use of that well-known URI; no OIDC-only claims are included).
 */
export function oauthMetadata(): Record<string, unknown> {
  const iss = oauthIssuer();
  return {
    issuer: iss,
    authorization_endpoint: `${iss}/oauth/authorize`,
    token_endpoint: `${iss}/oauth/token`,
    registration_endpoint: `${iss}/oauth/register`,
    scopes_supported: [SCOPE],
    response_types_supported: ["code"],
    grant_types_supported: ["authorization_code", "refresh_token"],
    token_endpoint_auth_methods_supported: ["none", "private_key_jwt", "client_secret_post"],
    code_challenge_methods_supported: ["S256"],
    authorization_response_iss_parameter_supported: true, // RFC 9207 §4.2
    // OpenAI ChatGPT prefers CIMD when advertised
    // (https://developers.openai.com/plugins/build/auth.md).
    client_id_metadata_document_supported: true,
    protected_resources: [oauthResourceUrl()], // RFC 9728 §4
  };
}

/** RFC 9728 protected-resource metadata; `resource` matches the URL used to fetch it. */
function protectedResourceMetadata(resource: string): Record<string, unknown> {
  return {
    resource,
    authorization_servers: [oauthIssuer()],
    scopes_supported: [SCOPE],
    bearer_methods_supported: ["header"],
    resource_name: "herdr-mcp",
  };
}

// ---------------------------------------------------------------------------
// Bearer auth for /mcp (static token + short-lived extension session + OAuth tokens)
// ---------------------------------------------------------------------------
/** Result of resolving a presented Bearer token (static or OAuth). */
export type AccessTokenInfo = {
  ok: boolean;
  /** OAuth client_id / JWT sub when known (CIMD URLs include chatgpt.com). */
  clientId?: string;
};

async function resolveAccessToken(token: string): Promise<AccessTokenInfo> {
  await jwtReady;
  // Preferred: JWT access token with aud=resource (OpenAI auth guide).
  if (token.includes(".") && jwtPublicKey) {
    try {
      const { payload } = await jwtVerify(token, jwtPublicKey, {
        issuer: oauthIssuer(),
      });
      const aud = payload.aud;
      const audOk = Array.isArray(aud)
        ? aud.includes(oauthResourceUrl()) || aud.includes(oauthIssuer())
        : aud === oauthResourceUrl() || aud === oauthIssuer();
      if (!audOk) return { ok: false };
      if (typeof payload.exp === "number" && payload.exp <= Math.floor(Date.now() / 1000)) {
        return { ok: false };
      }
      const clientId =
        (typeof payload.client_id === "string" && payload.client_id) ||
        (typeof payload.sub === "string" && payload.sub) ||
        undefined;
      return { ok: true, ...(clientId ? { clientId } : {}) };
    } catch {
      /* fall through to opaque legacy tokens */
    }
  }
  const h = hashToken(token);
  const entry = accessTokens.get(h);
  if (!entry) return { ok: false };
  if (Math.floor(Date.now() / 1000) > entry.expires_at) {
    accessTokens.delete(h);
    void persist("tokens");
    return { ok: false };
  }
  return { ok: true, clientId: entry.client_id };
}

/** True when an OAuth client_id belongs to ChatGPT CIMD (chatgpt.com). */
export function isChatgptOAuthClientId(clientId: string | undefined): boolean {
  if (!clientId) return false;
  try {
    if (!/^https:\/\//i.test(clientId)) return false;
    const host = new URL(clientId).hostname.toLowerCase();
    return host === "chatgpt.com" || host === "www.chatgpt.com";
  } catch {
    return false;
  }
}

/** Attach OAuth client_id on the request for downstream ChatGPT detection. */
export function getRequestOAuthClientId(req: Request): string | undefined {
  const v = (req as Request & { herdrOAuthClientId?: string }).herdrOAuthClientId;
  return typeof v === "string" ? v : undefined;
}

export function setRequestOAuthClientId(req: Request, clientId: string): void {
  (req as Request & { herdrOAuthClientId?: string }).herdrOAuthClientId = clientId;
}

/**
 * /mcp authorization: accepts the static HERDR_MCP_TOKEN (Claude / curl
 * compatibility), a valid short-lived extension-session bearer on loopback,
 * or a valid OAuth opaque access token. When
 * HERDR_MCP_TOKEN is unset the server stays open (previous dev behavior).
 * On rejection, challenges per RFC 9728 §5.1 so ChatGPT/OpenAI can discover
 * OAuth from the 401 alone.
 */
export function mcpBearerAuth(req: Request, res: Response, next: NextFunction): void {
  // Browser OAuth discovery (ChatGPT connector UI) sends CORS preflight to /mcp.
  // Never challenge OPTIONS — CORS middleware already answered or will fall through.
  if (req.method === "OPTIONS") {
    res.status(204).end();
    return;
  }
  if (!AUTH_TOKEN) {
    next();
    return;
  }
  void (async () => {
    const auth = req.get("authorization") ?? "";
    const m = /^Bearer\s+(.+)$/i.exec(auth);
    const presented = m ? m[1].trim() : "";
    const okStatic = presented !== "" && safeEqual(presented, AUTH_TOKEN);
    const okExtension = presented !== "" && !okStatic && extensionSessionBearerMatches(req);
    const oauth = presented !== "" && !okStatic && !okExtension ? await resolveAccessToken(presented) : { ok: false };
    if (!okStatic && !okExtension && !oauth.ok) {
      res.set(
        "WWW-Authenticate",
        `Bearer resource_metadata="${protectedResourceMetadataUrl()}", scope="${SCOPE}"`,
      );
      // Do not use JSON-RPC -32600 here: ChatGPT Connector surfaces that code as
      // "Session terminated". Auth failure is HTTP 401 + a distinct RPC code.
      res.status(401).json({
        jsonrpc: "2.0",
        error: { code: -32000, message: "unauthorized" },
      });
      return;
    }
    if (oauth.ok && oauth.clientId) {
      setRequestOAuthClientId(req, oauth.clientId);
    }
    next();
  })().catch((e) => {
    console.error("[herdr-mcp] mcpBearerAuth error:", e);
    res.status(500).json({ jsonrpc: "2.0", error: { code: -32603, message: "auth error" } });
  });
}

/** CORS for ChatGPT / Claude browser-side OAuth discovery and DCR. */
export function oauthCors(req: Request, res: Response, next: NextFunction): void {
  res.set("Access-Control-Allow-Origin", "*");
  res.set("Access-Control-Allow-Methods", "GET,HEAD,POST,OPTIONS");
  res.set(
    "Access-Control-Allow-Headers",
    "Authorization, Content-Type, Accept, Mcp-Session-Id, Mcp-Protocol-Version",
  );
  res.set("Access-Control-Expose-Headers", "WWW-Authenticate, Mcp-Session-Id");
  res.set("Access-Control-Max-Age", "86400");
  if (req.method === "OPTIONS") {
    res.status(204).end();
    return;
  }
  next();
}

/** ChatGPT CIMD redirect allowlist (OpenAI connector / platform callbacks). */
function isChatgptRedirectUri(uri: string): boolean {
  try {
    const u = new URL(uri);
    if (u.protocol !== "https:") return false;
    const host = u.hostname.toLowerCase();
    if (host !== "chatgpt.com" && host !== "www.chatgpt.com") return false;
    return (
      u.pathname === "/connector_platform_oauth_redirect" ||
      u.pathname.startsWith("/connector/oauth") ||
      u.pathname.startsWith("/oauth/")
    );
  } catch {
    return false;
  }
}

function isCimdClientId(clientId: string): boolean {
  return /^https:\/\//i.test(clientId);
}

/**
 * Resolve a DCR-registered client, or a ChatGPT CIMD client_id (HTTPS URL).
 * See https://developers.openai.com/plugins/build/auth.md — ChatGPT prefers CIMD
 * when client_id_metadata_document_supported is true.
 */
function resolveClient(clientId: string): StoredClient | undefined {
  const existing = clients.get(clientId);
  if (existing) return existing;
  if (!isCimdClientId(clientId)) return undefined;
  let host = "";
  try {
    host = new URL(clientId).hostname.toLowerCase();
  } catch {
    return undefined;
  }
  if (host !== "chatgpt.com" && host !== "www.chatgpt.com") return undefined;
  return {
    client_secret_hash: "",
    redirect_uris: [], // validated via isChatgptRedirectUri
    token_endpoint_auth_method: "none",
    grant_types: ["authorization_code", "refresh_token"],
    scope: SCOPE,
    client_name: "ChatGPT CIMD",
    issued_at: Math.floor(Date.now() / 1000),
  };
}

function redirectUriAllowed(client: StoredClient, redirectUri: string): boolean {
  if (client.redirect_uris.includes(redirectUri)) return true;
  if (client.redirect_uris.length === 0 && isChatgptRedirectUri(redirectUri)) return true;
  return false;
}

// ---------------------------------------------------------------------------
// Endpoint handlers
// ---------------------------------------------------------------------------
function first(v: unknown): string {
  if (Array.isArray(v)) return typeof v[0] === "string" ? v[0] : "";
  return typeof v === "string" ? v : "";
}

function authenticateClient(client: StoredClient, presentedSecret: string): boolean {
  if (client.token_endpoint_auth_method === "none") return true;
  if (!presentedSecret || !client.client_secret_hash) return false;
  return safeEqual(client.client_secret_hash, sha256Hex(presentedSecret));
}

function tokenError(res: Response, error: string, description: string): void {
  res.status(400).json({ error, error_description: description });
}

async function issueTokens(clientId: string, resource: string): Promise<Record<string, unknown>> {
  await jwtReady;
  if (!jwtPrivateKey) throw new Error("jwt private key not ready");
  const now = Math.floor(Date.now() / 1000);
  const jti = randomBytes(16).toString("hex");
  const at = await new SignJWT({
    client_id: clientId,
    scope: SCOPE,
  })
    .setProtectedHeader({ alg: "RS256", typ: "at+jwt" })
    .setIssuer(oauthIssuer())
    .setAudience(resource)
    .setSubject(clientId)
    .setJti(jti)
    .setIssuedAt(now)
    .setExpirationTime(now + ACCESS_TOKEN_TTL_S)
    .sign(jwtPrivateKey);
  const rt = newToken();
  // Keep a jti index for ops/debug; verification is cryptographic (JWT).
  accessTokens.set(jti, {
    client_id: clientId, resource, scope: SCOPE, expires_at: now + ACCESS_TOKEN_TTL_S,
  });
  refreshTokens.set(hashToken(rt), {
    client_id: clientId, resource, scope: SCOPE, expires_at: now + REFRESH_TOKEN_TTL_S,
  });
  await persist("tokens");
  await persist("refresh");
  return {
    access_token: at,
    token_type: "Bearer",
    expires_in: ACCESS_TOKEN_TTL_S,
    refresh_token: rt,
    scope: SCOPE,
  };
}

/** RFC 7591 dynamic client registration. */
async function handleRegister(req: Request, res: Response): Promise<void> {
  res.set("Cache-Control", "no-store");
  const body = (req.body ?? {}) as Record<string, unknown>;
  const redirectUrisRaw = body["redirect_uris"];
  const redirectUris = Array.isArray(redirectUrisRaw)
    ? redirectUrisRaw.filter((u): u is string => typeof u === "string")
    : [];
  if (redirectUris.length === 0) {
    tokenError(res, "invalid_client_metadata", "redirect_uris is required");
    return;
  }
  const requestedGrants = Array.isArray(body["grant_types"])
    ? (body["grant_types"] as unknown[]).filter((g): g is string => typeof g === "string")
    : ["authorization_code", "refresh_token"];
  const grantTypes = requestedGrants.filter(
    (g) => g === "authorization_code" || g === "refresh_token",
  );
  const authMethod: "none" | "client_secret_post" =
    body["token_endpoint_auth_method"] === "client_secret_post" ? "client_secret_post" : "none";
  const scope = typeof body["scope"] === "string" && body["scope"] !== "" ? body["scope"] : SCOPE;
  const clientName = typeof body["client_name"] === "string" ? body["client_name"] : undefined;

  const clientId = `dcr-${randomBytes(8).toString("hex")}`;
  const clientSecret = randomBytes(32).toString("base64url");
  clients.set(clientId, {
    client_secret_hash: sha256Hex(clientSecret),
    redirect_uris: redirectUris,
    token_endpoint_auth_method: authMethod,
    grant_types: grantTypes,
    scope,
    ...(clientName !== undefined ? { client_name: clientName } : {}),
    issued_at: Math.floor(Date.now() / 1000),
  });
  await persist("clients");

  res.status(201).json({
    client_id: clientId,
    client_secret: clientSecret,
    ...(clientName !== undefined ? { client_name: clientName } : {}),
    redirect_uris: redirectUris,
    grant_types: grantTypes,
    token_endpoint_auth_method: authMethod,
    scope,
    client_id_issued_at: Math.floor(Date.now() / 1000),
    client_secret_expires_at: 0, // never expires
  });
}

/**
 * Authorization endpoint (auto-completes; herdr-mcp has no user login — the
 * static HERDR_MCP_TOKEN is the real secret). Requires a registered
 * client_id, a registered redirect_uri, and PKCE S256. RFC 9207 `iss` is
 * added to both success and error responses.
 */
function handleAuthorize(req: Request, res: Response): void {
  const q = req.query;
  const redirectUri = typeof q["redirect_uri"] === "string" ? q["redirect_uri"] : "";
  const state = typeof q["state"] === "string" ? q["state"] : "";
  const clientId = typeof q["client_id"] === "string" ? q["client_id"] : "";
  const responseType = typeof q["response_type"] === "string" ? q["response_type"] : "code";
  const codeChallenge = typeof q["code_challenge"] === "string" ? q["code_challenge"] : "";
  const codeChallengeMethod =
    typeof q["code_challenge_method"] === "string" ? q["code_challenge_method"] : "S256";
  const scope = typeof q["scope"] === "string" ? q["scope"] : "";

  const client = resolveClient(clientId);
  if (!client) {
    // RFC 6749 §4.1.2.1: unknown client / unverifiable redirect_uri → no redirect.
    res.status(400).json({ error: "invalid_request", error_description: "unknown client_id" });
    return;
  }
  if (!redirectUri || !redirectUriAllowed(client, redirectUri)) {
    res.status(400).json({
      error: "invalid_request",
      error_description: "redirect_uri is not registered for this client",
    });
    return;
  }

  const redirectError = (error: string, description: string): void => {
    const sep = redirectUri.includes("?") ? "&" : "?";
    const params = new URLSearchParams({ error, error_description: description });
    if (state) params.set("state", state);
    params.set("iss", oauthIssuer()); // RFC 9207 §4 also covers error responses
    res.redirect(302, `${redirectUri}${sep}${params.toString()}`);
  };

  if (responseType !== "code") {
    redirectError("unsupported_response_type", "only response_type=code is supported");
    return;
  }
  if (!codeChallenge) {
    redirectError("invalid_request", "PKCE code_challenge is required (S256)");
    return;
  }
  if (codeChallengeMethod !== "S256") {
    redirectError("invalid_request", "only code_challenge_method=S256 is supported");
    return;
  }
  if (scope && scope !== SCOPE) {
    redirectError("invalid_scope", `unsupported scope '${scope}'`);
    return;
  }
  const resource = normalizeResource(typeof q["resource"] === "string" ? q["resource"] : "");
  if (!resource) {
    redirectError("invalid_target", "unsupported resource");
    return;
  }

  const code = randomBytes(24).toString("base64url");
  codes.set(code, {
    client_id: clientId,
    redirect_uri: redirectUri,
    code_challenge: codeChallenge,
    resource,
    expires_at: Date.now() + CODE_TTL_MS,
  });
  const sep = redirectUri.includes("?") ? "&" : "?";
  const params = new URLSearchParams({ code, iss: oauthIssuer() }); // RFC 9207
  if (state) params.set("state", state);
  res.redirect(302, `${redirectUri}${sep}${params.toString()}`);
}

/** RFC 6749 token endpoint: authorization_code + refresh_token (rotating). */
async function handleToken(req: Request, res: Response): Promise<void> {
  res.set("Cache-Control", "no-store");
  const body = (req.body ?? {}) as Record<string, unknown>;
  let clientId = first(body["client_id"]);
  let presentedSecret = first(body["client_secret"]);
  const grantType = first(body["grant_type"]);

  // RFC 6749 §2.3.1 HTTP Basic client authentication fallback.
  const authHeader = req.get("authorization") ?? "";
  if (authHeader.startsWith("Basic ")) {
    try {
      const decoded = Buffer.from(authHeader.slice(6), "base64").toString("utf8");
      const idx = decoded.indexOf(":");
      if (idx >= 0) {
        if (!clientId) clientId = decodeURIComponent(decoded.slice(0, idx));
        if (!presentedSecret) presentedSecret = decodeURIComponent(decoded.slice(idx + 1));
      }
    } catch {
      /* malformed Basic header — ignore */
    }
  }

  const resource = normalizeResource(first(body["resource"]));
  if (!resource) {
    tokenError(res, "invalid_target", `resource must be ${oauthIssuer()} or ${oauthResourceUrl()}`);
    return;
  }
  const client = clientId ? resolveClient(clientId) : undefined;
  if (!clientId || !client) {
    tokenError(res, "invalid_client", "unknown client_id");
    return;
  }

  // CIMD private_key_jwt (OpenAI): verify client_assertion against ChatGPT JWKS.
  const assertion = first(body["client_assertion"]);
  const assertionType = first(body["client_assertion_type"]);
  if (assertion) {
    if (
      assertionType &&
      assertionType !== "urn:ietf:params:oauth:client-assertion-type:jwt-bearer"
    ) {
      tokenError(res, "invalid_client", "unsupported client_assertion_type");
      return;
    }
    try {
      let jwksUrl: URL;
      if (isCimdClientId(clientId)) {
        jwksUrl = new URL("/oauth/jwks.json", new URL(clientId).origin);
      } else {
        tokenError(res, "invalid_client", "client_assertion requires CIMD client_id");
        return;
      }
      const JWKS = createRemoteJWKSet(jwksUrl);
      const { payload } = await jwtVerify(assertion, JWKS, {
        issuer: clientId,
        audience: [`${oauthIssuer()}/oauth/token`, oauthIssuer()],
      });
      const sub = typeof payload.sub === "string" ? payload.sub : "";
      if (sub && sub !== clientId) {
        tokenError(res, "invalid_client", "client_assertion sub mismatch");
        return;
      }
    } catch (e) {
      console.error("[herdr-mcp] oauth client_assertion verify failed:", e instanceof Error ? e.message : e);
      tokenError(res, "invalid_client", "client_assertion verification failed");
      return;
    }
  } else if (!authenticateClient(client, presentedSecret)) {
    tokenError(res, "invalid_client", "client authentication failed");
    return;
  }

  console.log(
    `[herdr-mcp] oauth/token grant=${grantType || "-"} client=${clientId.slice(0, 64)}` +
      ` assertion=${assertion ? "yes" : "no"} resource=${resource}`,
  );

  if (grantType === "authorization_code") {
    const code = first(body["code"]);
    const redirectUri = first(body["redirect_uri"]);
    const codeVerifier = first(body["code_verifier"]);
    const scope = first(body["scope"]);
    if (scope && scope !== SCOPE) {
      tokenError(res, "invalid_scope", `unsupported scope '${scope}'`);
      return;
    }
    if (!code) {
      tokenError(res, "invalid_grant", "code is required");
      return;
    }
    const entry = codes.get(code);
    codes.delete(code); // one-use: consumed before validation (defeats replay racing)
    if (!entry) {
      tokenError(res, "invalid_grant", "unknown or already-used authorization code");
      return;
    }
    if (Date.now() > entry.expires_at) {
      tokenError(res, "invalid_grant", "authorization code expired");
      return;
    }
    if (!safeEqual(entry.client_id, clientId)) {
      tokenError(res, "invalid_grant", "code was issued to a different client");
      return;
    }
    if (!safeEqual(entry.redirect_uri, redirectUri)) {
      tokenError(res, "invalid_grant", "redirect_uri mismatch");
      return;
    }
    if (!safeEqual(entry.resource, resource)) {
      tokenError(res, "invalid_target", "resource mismatch");
      return;
    }
    if (!verifyPkce(codeVerifier, entry.code_challenge)) {
      tokenError(res, "invalid_grant", "PKCE verification failed");
      return;
    }
    res.status(200).json(await issueTokens(clientId, resource));
    return;
  }

  if (grantType === "refresh_token") {
    const rt = first(body["refresh_token"]);
    if (!rt) {
      tokenError(res, "invalid_grant", "refresh_token is required");
      return;
    }
    const key = hashToken(rt);
    const entry = refreshTokens.get(key);
    refreshTokens.delete(key); // rotation: old refresh token rejected on reuse
    if (!entry) {
      tokenError(res, "invalid_grant", "unknown, expired or already-used refresh_token");
      return;
    }
    if (Math.floor(Date.now() / 1000) > entry.expires_at) {
      tokenError(res, "invalid_grant", "refresh_token expired");
      return;
    }
    if (!safeEqual(entry.client_id, clientId)) {
      tokenError(res, "invalid_grant", "refresh_token belongs to a different client");
      return;
    }
    if (!safeEqual(entry.resource, resource)) {
      tokenError(res, "invalid_target", "resource mismatch");
      return;
    }
    res.status(200).json(await issueTokens(clientId, resource));
    return;
  }

  tokenError(res, "unsupported_grant_type", `unsupported grant_type '${grantType}'`);
}

// ---------------------------------------------------------------------------
// Route mounting
// ---------------------------------------------------------------------------
export function registerOAuthRoutes(app: Express): void {
  void jwtReady;
  const router = Router();

  // RFC 8414 authorization-server metadata.
  router.get("/.well-known/oauth-authorization-server", (_req, res) => {
    res.status(200).json(oauthMetadata());
  });
  // RFC 8414 §5 compatibility: OAuth-only AS metadata at the OIDC discovery
  // URL (ChatGPT's client probes this after RFC 8414 discovery). No OIDC
  // claims — this document is OAuth 2.0 metadata.
  router.get("/.well-known/openid-configuration", (_req, res) => {
    res.status(200).json(oauthMetadata());
  });
  // Path-aware aliases (RFC 8414 §4): when ChatGPT uses `…/mcp` as the server
  // URL it probes discovery at `/.well-known/oauth-authorization-server/mcp`,
  // `/.well-known/openid-configuration/mcp`, and the `/mcp/.well-known/…`
  // forms. Without these the paths fell through to mcpBearerAuth -> 401 and
  // ChatGPT judged the server as "does not implement OAuth". All return the
  // same oauthMetadata() as the root document.
  router.get("/.well-known/oauth-authorization-server/mcp", (_req, res) => {
    res.status(200).json(oauthMetadata());
  });
  router.get("/.well-known/openid-configuration/mcp", (_req, res) => {
    res.status(200).json(oauthMetadata());
  });
  router.get("/mcp/.well-known/oauth-authorization-server", (_req, res) => {
    res.status(200).json(oauthMetadata());
  });
  router.get("/mcp/.well-known/openid-configuration", (_req, res) => {
    res.status(200).json(oauthMetadata());
  });
  // RFC 9728 protected-resource metadata: root form (resource = issuer) …
  router.get("/.well-known/oauth-protected-resource", (_req, res) => {
    res.status(200).json(protectedResourceMetadata(oauthIssuer()));
  });
  // … and path-aware form (resource = issuer + /mcp). `resource` always
  // matches the URL the client used to fetch the document (RFC 9728 §3.3).
  router.get("/.well-known/oauth-protected-resource/mcp", (_req, res) => {
    res.status(200).json(protectedResourceMetadata(oauthResourceUrl()));
  });
  // Path-aware protected-resource form (RFC 9728 §5.1) for the `/mcp` URL.
  router.get("/mcp/.well-known/oauth-protected-resource", (_req, res) => {
    res.status(200).json(protectedResourceMetadata(oauthResourceUrl()));
  });
  // MCP server card (serverUrl required by MCP 2025-06-18+ spec).
  router.get("/.well-known/mcp.json", (_req, res) => {
    res.status(200).json({ serverUrl: oauthResourceUrl(), name: SERVER_NAME, version: SERVER_VERSION });
  });

  router.post("/oauth/register", (req, res) => {
    void handleRegister(req, res);
  });
  // ChatGPT DCR fallback: some clients POST to `/register` (no `/oauth`
  // prefix) instead of the advertised `/oauth/register`. Serve the same
  // handler on both the canonical and trailing-slash forms. These are OAuth
  // routes registered BEFORE the MCP bearer-auth middleware, so they are not
  // 401'd by mcpBearerAuth.
  router.post("/register", (req, res) => {
    void handleRegister(req, res);
  });
  router.post("/register/", (req, res) => {
    void handleRegister(req, res);
  });
  // When the connector URL is `…/mcp`, some OpenAI clients resolve DCR
  // relative to that path → POST /mcp/register (historically 401 via MCP auth).
  router.post("/mcp/register", (req, res) => {
    void handleRegister(req, res);
  });
  router.post("/mcp/register/", (req, res) => {
    void handleRegister(req, res);
  });
  router.post("/mcp/oauth/register", (req, res) => {
    void handleRegister(req, res);
  });
  router.get("/oauth/authorize", handleAuthorize);
  router.post("/oauth/token", (req, res) => {
    void handleToken(req, res);
  });

  app.use(router);
}

console.log(
  `[herdr-mcp] oauth issuer=${oauthIssuer()} resource=${oauthResourceUrl()} store=${OAUTH_DIR}`,
);
