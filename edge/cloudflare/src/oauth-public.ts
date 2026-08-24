/**
 * oauth-public.ts — public OAuth HTTP compatibility handler (Phase 4).
 *
 * This module turns the EXTERNAL HTTP behavior of `src/oauth.ts` (RFC 8414 /
 * RFC 9728 discovery, RFC 7591 DCR, authorization-code + PKCE, rotating
 * refresh, ChatGPT CIMD private_key_jwt) into a single, self-contained
 * Cloudflare-Worker-callable handler:
 *
 *     const response = await handleOAuthPublic(request, options);
 *     if (response) return response;   // owned by this handler
 *     // else: falls through to index.ts routing
 *
 * Design boundaries:
 *  - NO secondary state. All OAuth state (clients, authorization codes,
 *    refresh tokens, signing key) lives in the existing `OAUTH_STORE_DO`
 *    Durable Object, reached exclusively through its internal HTTP API via
 *    the injected `OAuthPublicStore` adapter `createOAuthPublicStore(stub)`.
 *  - Access tokens are issued by the DO (`/internal/oauth/token/issue`) as
 *    RS256 JWTs aud=resource, mirroring `src/oauth.ts` `issueTokens`.
 *  - Refresh rotation uses the DO's atomic `/internal/oauth/refresh/exchange`
 *    (consume + ownership check + new issue), never a fiddly two-step.
 *  - PKCE and ChatGPT private_key_jwt verification come from
 *    `oauth-token-crypto.ts` (zero-dep Web Crypto); JWKS fetching uses the
 *    injected `fetchFn` so tests never touch the network.
 *  - No filesystem, no process env, no jose, no node:crypto — runs
 *    identically under Node tests and the Workers runtime.
 *
 * Route set and error strings mirror `src/oauth.ts` as closely as a durable
 * store allows. Unavoidable differences (reported):
 *  - Authorization codes are stored/consumed under `hashOpaqueToken(code)` in
 *    the DO instead of raw strings in a process-memory map.
 *  - Refresh-token failures (missing / expired / wrong client / wrong
 *    resource) collapse to the DO's generic `invalid_grant` — the atomic
 *    exchange does not distinguish them the way the in-memory handler did.
 *  - Explicit body/query/param bounds add 413/400 behavior that the older
 *    Express handler (10 MB body limit, no query cap) did not enforce.
 *  - The DO returns an internal `key_id` on issued token pairs; it is
 *    stripped so the public payload matches the old HTTP response exactly.
 */

import {
  OAUTH_SCOPE,
  hashOpaqueToken,
  mcpServerCardMetadata,
  normalizeResource,
  oauthEdgeMetadata,
  protectedResourceEdgeMetadata,
  type OAuthEdgeIdentity,
} from "./oauth-edge.js";
import {
  isChatgptOAuthClientId,
  randomBase64UrlToken,
  verifyChatgptPrivateKeyJwt,
  verifyPkceS256,
} from "./oauth-token-crypto.js";
import type { OAuthClientRecord, OAuthCodeRecord } from "./oauth-store-do.js";
import { MCP_SERVER_VERSION } from "./version.js";

// ---------------------------------------------------------------------------
// Options / dependency types
// ---------------------------------------------------------------------------

/** A minimal durable-object stub: anything exposing `.fetch(Request)` works. */
export interface DoStub {
  fetch(request: Request): Promise<Response>;
}

/** Result of consuming a one-use authorization code from the DO. */
export type ConsumeCodeResult =
  | { ok: true; record: OAuthCodeRecord }
  | { ok: false; code: "not_found" | "expired" };

/** Public token response — identical shape to src/oauth.ts `issueTokens`. */
export interface IssuedTokenPair {
  access_token: string;
  token_type: string;
  expires_in: number;
  refresh_token: string;
  scope: string;
}

export interface TokenIssueInput {
  client_id: string;
  resource: string;
  now_sec: number;
  access_ttl_sec: number;
  refresh_ttl_sec: number;
}

export type RefreshExchangeInput = TokenIssueInput & { hash: string };

/**
 * The full store surface this handler needs. Backed by `createOAuthPublicStore`
 * in production; tests may inject their own in-memory implementation, but the
 * recommended harness wraps a real `OAuthStoreDO` instance over a StorageMock
 * so the internal HTTP API itself is exercised.
 */
export interface OAuthPublicStore {
  getClient(clientId: string): Promise<OAuthClientRecord | null>;
  putClient(clientId: string, record: OAuthClientRecord): Promise<boolean>;
  putCode(hash: string, record: OAuthCodeRecord, nowMs: number): Promise<boolean>;
  consumeCode(hash: string, nowMs: number): Promise<ConsumeCodeResult>;
  issueTokens(input: TokenIssueInput): Promise<IssuedTokenPair | null>;
  exchangeRefresh(input: RefreshExchangeInput): Promise<IssuedTokenPair | null>;
}

export interface OAuthPublicOptions {
  /** Exact production issuer/resource identity (see createOAuthIdentity). */
  identity: OAuthEdgeIdentity;
  store: OAuthPublicStore;
  /** Injected JWKS fetch for ChatGPT CIMD assertions (default globalThis.fetch). */
  fetchFn?: typeof globalThis.fetch;
  /** Injectable clock, default Date.now. */
  nowMs?: () => number;
  serverName?: string;
  serverVersion?: string;
  accessTokenTtlSec?: number;
  refreshTokenTtlSec?: number;
  codeTtlMs?: number;
  maxBodyBytes?: number;
  maxQueryBytes?: number;
  maxParamBytes?: number;
}

// ---------------------------------------------------------------------------
// Defaults (mirror src/oauth.ts constants)
// ---------------------------------------------------------------------------

const DEFAULT_SERVER_NAME = "herdr-mcp";
const DEFAULT_SERVER_VERSION = MCP_SERVER_VERSION;
const DEFAULT_ACCESS_TTL_S = 86400;
const DEFAULT_REFRESH_TTL_S = 2592000; // 30 days
const DEFAULT_CODE_TTL_MS = 5 * 60_000;
const DEFAULT_MAX_BODY_BYTES = 256 * 1024;
const DEFAULT_MAX_QUERY_BYTES = 16 * 1024;
const DEFAULT_MAX_PARAM_BYTES = 4096;
const JWT_BEARER = "urn:ietf:params:oauth:client-assertion-type:jwt-bearer";

// Route table (exact paths from src/oauth.ts).
const METADATA_PATHS = new Set<string>([
  "/.well-known/oauth-authorization-server",
  "/.well-known/oauth-authorization-server/mcp",
  "/.well-known/openid-configuration",
  "/.well-known/openid-configuration/mcp",
  "/mcp/.well-known/oauth-authorization-server",
  "/mcp/.well-known/openid-configuration",
]);

/** Which resource each protected-resource path serves (RFC 9728 §3.3). */
const PROTECTED_RESOURCE_PATHS = new Map<string, "issuer" | "resource">([
  ["/.well-known/oauth-protected-resource", "issuer"],
  ["/.well-known/oauth-protected-resource/mcp", "resource"],
  ["/mcp/.well-known/oauth-protected-resource", "resource"],
]);

const REGISTER_PATHS = new Set<string>([
  "/oauth/register",
  "/register",
  "/register/",
  "/mcp/register",
  "/mcp/register/",
  "/mcp/oauth/register",
]);

const MCP_JSON_PATH = "/.well-known/mcp.json";
const AUTHORIZE_PATH = "/oauth/authorize";
const TOKEN_PATH = "/oauth/token";

function isOwnedPath(path: string): boolean {
  return (
    METADATA_PATHS.has(path) ||
    PROTECTED_RESOURCE_PATHS.has(path) ||
    REGISTER_PATHS.has(path) ||
    path === MCP_JSON_PATH ||
    path === AUTHORIZE_PATH ||
    path === TOKEN_PATH
  );
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

const enc = new TextEncoder();

function corsHeaders(): Record<string, string> {
  return {
    "access-control-allow-origin": "*",
    "access-control-allow-methods": "GET,HEAD,POST,OPTIONS",
    "access-control-allow-headers":
      "Authorization, Content-Type, Accept, Mcp-Session-Id, Mcp-Protocol-Version",
    "access-control-expose-headers": "WWW-Authenticate, Mcp-Session-Id",
    "access-control-max-age": "86400",
  };
}

function timingSafeStringEqual(a: string, b: string): boolean {
  const aBytes = enc.encode(a);
  const bBytes = enc.encode(b);
  if (aBytes.length !== bBytes.length) return false;
  let diff = 0;
  for (let i = 0; i < aBytes.length; i++) diff |= aBytes[i] ^ bBytes[i];
  return diff === 0;
}

async function sha256Hex(s: string): Promise<string> {
  const digest = await crypto.subtle.digest("SHA-256", enc.encode(s));
  return Array.from(new Uint8Array(digest))
    .map((b) => b.toString(16).padStart(2, "0"))
    .join("");
}

function randomHex(bytes: number): string {
  const buf = new Uint8Array(bytes);
  crypto.getRandomValues(buf);
  return Array.from(buf)
    .map((b) => b.toString(16).padStart(2, "0"))
    .join("");
}

function firstOf(v: unknown): string {
  if (Array.isArray(v)) return typeof v[0] === "string" ? v[0] : "";
  return typeof v === "string" ? v : "";
}

/** Build an object with same first-value semantics as Express req.query/body. */
function paramsObject(entries: Iterable<[string, string]>): Record<string, string | string[]> {
  const out: Record<string, string | string[]> = {};
  for (const [k, v] of entries) {
    if (k in out) {
      const cur = out[k];
      if (Array.isArray(cur)) cur.push(v);
      else out[k] = [cur, v];
    } else {
      out[k] = v;
    }
  }
  return out;
}

type BodyResult =
  | { ok: true; value: Record<string, unknown> }
  | { ok: false; code: "payload_too_large" | "bad_request" };

/**
 * Read a bounded request body with Express-shaped semantics: empty body → {},
 * `application/json` → parsed JSON, `application/x-www-form-urlencoded` →
 * first-value object, unknown content-type → {}. Oversized → payload_too_large.
 */
async function readRequestBody(request: Request, maxBytes: number): Promise<BodyResult> {
  const declared = request.headers.get("content-length");
  if (declared !== null) {
    const n = Number.parseInt(declared, 10);
    if (Number.isFinite(n) && n > maxBytes) return { ok: false, code: "payload_too_large" };
  }
  let text: string;
  try {
    text = await request.text();
  } catch {
    return { ok: false, code: "bad_request" };
  }
  if (new TextEncoder().encode(text).byteLength > maxBytes) {
    return { ok: false, code: "payload_too_large" };
  }
  if (text.trim() === "") return { ok: true, value: {} };
  const ctype = (request.headers.get("content-type") ?? "").toLowerCase();
  if (ctype.includes("application/json") || ctype.includes("+json")) {
    try {
      const value = JSON.parse(text) as unknown;
      if (value !== null && typeof value === "object" && !Array.isArray(value)) {
        return { ok: true, value: value as Record<string, unknown> };
      }
      return { ok: false, code: "bad_request" };
    } catch {
      return { ok: false, code: "bad_request" };
    }
  }
  if (
    ctype.includes("application/x-www-form-urlencoded") ||
    ctype === "" ||
    ctype.includes("text/plain")
  ) {
    try {
      const params = new URLSearchParams(text);
      return { ok: true, value: paramsObject(params) };
    } catch {
      return { ok: false, code: "bad_request" };
    }
  }
  return { ok: true, value: {} };
}

// ---------------------------------------------------------------------------
// Client resolution + redirect allowlist (mirrors src/oauth.ts)
// ---------------------------------------------------------------------------

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

function redirectUriAllowed(client: OAuthClientRecord, redirectUri: string): boolean {
  if (client.redirect_uris.includes(redirectUri)) return true;
  if (client.redirect_uris.length === 0 && isChatgptRedirectUri(redirectUri)) return true;
  return false;
}

/**
 * Resolve a registered client, or a ChatGPT CIMD client_id (HTTPS URL on
 * chatgpt.com). CIMD clients are synthesized exactly like src/oauth.ts and are
 * validated against the chatgpt redirect allowlist, not the store.
 */
async function resolveClient(
  clientId: string,
  store: OAuthPublicStore,
  nowSec: number,
): Promise<OAuthClientRecord | null> {
  const existing = await store.getClient(clientId);
  if (existing) return existing;
  if (!isChatgptOAuthClientId(clientId)) return null;
  return {
    client_secret_hash: "",
    redirect_uris: [],
    token_endpoint_auth_method: "none",
    grant_types: ["authorization_code", "refresh_token"],
    scope: OAUTH_SCOPE,
    client_name: "ChatGPT CIMD",
    issued_at: nowSec,
  };
}

async function authenticateClient(
  client: OAuthClientRecord,
  presentedSecret: string,
): Promise<boolean> {
  if (client.token_endpoint_auth_method === "none") return true;
  if (!presentedSecret || !client.client_secret_hash) return false;
  const hash = await sha256Hex(presentedSecret);
  return timingSafeStringEqual(client.client_secret_hash, hash);
}

// ---------------------------------------------------------------------------
// Endpoint handlers
// ---------------------------------------------------------------------------

interface HandlerCtx {
  identity: OAuthEdgeIdentity;
  store: OAuthPublicStore;
  fetchFn: typeof globalThis.fetch;
  nowMs: () => number;
  accessTtlSec: number;
  refreshTtlSec: number;
  codeTtlMs: number;
  maxBodyBytes: number;
  maxQueryBytes: number;
  maxParamBytes: number;
  cors: Record<string, string>;
  json: (payload: unknown, status?: number, extraHeaders?: Record<string, string>) => Response;
}

function tokenError(
  ctx: HandlerCtx,
  error: string,
  description: string,
  status = 400,
): Response {
  return ctx.json({ error, error_description: description }, status, { "cache-control": "no-store" });
}

/** Unexpected store/issuance failure — never leaks internal response details. */
function serverError(ctx: HandlerCtx, description: string): Response {
  return ctx.json({ error: "server_error", error_description: description }, 500, { "cache-control": "no-store" });
}

/** RFC 7591 dynamic client registration. */
async function handleRegister(request: Request, ctx: HandlerCtx): Promise<Response> {
  const bodyResult = await readRequestBody(request, ctx.maxBodyBytes);
  if (!bodyResult.ok) {
    return tokenError(
      ctx,
      "invalid_request",
      bodyResult.code === "payload_too_large" ? "request body too large" : "invalid request body",
      bodyResult.code === "payload_too_large" ? 413 : 400,
    );
  }
  const body = bodyResult.value;
  const redirectUrisRaw = body["redirect_uris"];
  const redirectUris = Array.isArray(redirectUrisRaw)
    ? redirectUrisRaw
        .filter((u): u is string => typeof u === "string" && u.length > 0 && u.length <= ctx.maxParamBytes)
        .slice(0, 32) // DO normalizer caps at 32
    : [];
  if (redirectUris.length === 0) {
    return tokenError(ctx, "invalid_client_metadata", "redirect_uris is required");
  }
  const requestedGrants = Array.isArray(body["grant_types"])
    ? (body["grant_types"] as unknown[]).filter((g): g is string => typeof g === "string")
    : ["authorization_code", "refresh_token"];
  const grantTypes = requestedGrants.filter(
    (g) => g === "authorization_code" || g === "refresh_token",
  );
  const authMethod: "none" | "client_secret_post" =
    body["token_endpoint_auth_method"] === "client_secret_post" ? "client_secret_post" : "none";
  const scope =
    typeof body["scope"] === "string" && body["scope"] !== "" && body["scope"].length <= ctx.maxParamBytes
      ? body["scope"]
      : OAUTH_SCOPE;
  const clientName =
    typeof body["client_name"] === "string" && body["client_name"].length <= ctx.maxParamBytes
      ? body["client_name"]
      : undefined;

  const clientId = `dcr-${randomHex(8)}`;
  const clientSecret = randomBase64UrlToken();
  const nowSec = Math.floor(ctx.nowMs() / 1000);
  const persisted = await ctx.store.putClient(clientId, {
    client_secret_hash: await sha256Hex(clientSecret),
    redirect_uris: redirectUris,
    token_endpoint_auth_method: authMethod,
    grant_types: grantTypes,
    scope,
    ...(clientName !== undefined ? { client_name: clientName } : {}),
    issued_at: nowSec,
  });
  if (!persisted) {
    return serverError(ctx, "client registration failed");
  }

  return ctx.json(
    {
      client_id: clientId,
      client_secret: clientSecret,
      ...(clientName !== undefined ? { client_name: clientName } : {}),
      redirect_uris: redirectUris,
      grant_types: grantTypes,
      token_endpoint_auth_method: authMethod,
      scope,
      client_id_issued_at: nowSec,
      client_secret_expires_at: 0, // never expires
    },
    201,
    { "cache-control": "no-store" },
  );
}

/**
 * Authorization endpoint (auto-completes; RFC 9207 `iss` on success and error
 * redirects). Requires registered client, allowlisted redirect_uri, PKCE S256.
 */
async function handleAuthorize(url: URL, ctx: HandlerCtx): Promise<Response> {
  if (url.search.length > ctx.maxQueryBytes) {
    return ctx.json({ error: "invalid_request", error_description: "authorization request too large" }, 400);
  }
  const q = paramsObject(url.searchParams);
  const get = (k: string): string => firstOf(q[k]);
  const redirectUri = get("redirect_uri");
  const state = get("state");
  const clientId = get("client_id");
  const responseType = get("response_type") || "code";
  const codeChallenge = get("code_challenge");
  const codeChallengeMethod = get("code_challenge_method") || "S256";
  const scopeParam = get("scope");
  const resourceParam = get("resource");

  // Bounds on anything echoed into a redirect or matched against a client.
  if (
    redirectUri.length > ctx.maxParamBytes ||
    state.length > ctx.maxParamBytes ||
    clientId.length > ctx.maxParamBytes ||
    codeChallenge.length > ctx.maxParamBytes
  ) {
    return ctx.json({ error: "invalid_request", error_description: "authorization parameter too large" }, 400);
  }

  const nowMs = ctx.nowMs();
  const nowSec = Math.floor(nowMs / 1000);
  const client = await resolveClient(clientId, ctx.store, nowSec);
  if (!client) {
    // RFC 6749 §4.1.2.1: unknown client / unverifiable redirect_uri → no redirect.
    return ctx.json({ error: "invalid_request", error_description: "unknown client_id" }, 400);
  }
  if (!redirectUri || !redirectUriAllowed(client, redirectUri)) {
    return ctx.json(
      { error: "invalid_request", error_description: "redirect_uri is not registered for this client" },
      400,
    );
  }

  const redirect = (params: URLSearchParams): Response => {
    const sep = redirectUri.includes("?") ? "&" : "?";
    return new Response(null, {
      status: 302,
      headers: { ...ctx.cors, location: `${redirectUri}${sep}${params.toString()}` },
    });
  };
  const redirectError = (error: string, description: string): Response => {
    const params = new URLSearchParams({ error, error_description: description });
    if (state) params.set("state", state);
    params.set("iss", ctx.identity.issuer); // RFC 9207 §4 also covers error responses
    return redirect(params);
  };

  if (responseType !== "code") {
    return redirectError("unsupported_response_type", "only response_type=code is supported");
  }
  if (!codeChallenge) {
    return redirectError("invalid_request", "PKCE code_challenge is required (S256)");
  }
  if (codeChallengeMethod !== "S256") {
    return redirectError("invalid_request", "only code_challenge_method=S256 is supported");
  }
  if (scopeParam && scopeParam !== OAUTH_SCOPE) {
    return redirectError("invalid_scope", `unsupported scope '${scopeParam}'`);
  }
  const resource = normalizeResource(ctx.identity, resourceParam ?? "");
  if (!resource) {
    return redirectError("invalid_target", "unsupported resource");
  }

  const code = randomBase64UrlToken();
  const codeHash = await hashOpaqueToken(code);
  const persisted = await ctx.store.putCode(
    codeHash,
    {
      client_id: clientId,
      redirect_uri: redirectUri,
      code_challenge: codeChallenge,
      resource,
      expires_at: nowMs + ctx.codeTtlMs,
    },
    nowMs,
  );
  if (!persisted) {
    return ctx.json(
      { error: "server_error", error_description: "authorization code issuance failed" },
      500,
      { "cache-control": "no-store" },
    );
  }
  const params = new URLSearchParams({ code, iss: ctx.identity.issuer }); // RFC 9207
  if (state) params.set("state", state);
  return redirect(params);
}

/** RFC 6749 token endpoint: authorization_code + refresh_token (rotating). */
async function handleToken(request: Request, ctx: HandlerCtx): Promise<Response> {
  const bodyResult = await readRequestBody(request, ctx.maxBodyBytes);
  if (!bodyResult.ok) {
    return tokenError(
      ctx,
      "invalid_request",
      bodyResult.code === "payload_too_large" ? "request body too large" : "invalid request body",
      bodyResult.code === "payload_too_large" ? 413 : 400,
    );
  }
  const body = bodyResult.value;
  const first = (k: string): string => firstOf(body[k]);
  let clientId = first("client_id");
  let presentedSecret = first("client_secret");
  const grantType = first("grant_type");

  // RFC 6749 §2.3.1 HTTP Basic client authentication fallback.
  const authHeader = request.headers.get("authorization") ?? "";
  if (authHeader.startsWith("Basic ")) {
    try {
      const decoded = atob(authHeader.slice(6).trim());
      const idx = decoded.indexOf(":");
      if (idx >= 0) {
        if (!clientId) clientId = decodeURIComponent(decoded.slice(0, idx));
        if (!presentedSecret) presentedSecret = decodeURIComponent(decoded.slice(idx + 1));
      }
    } catch {
      /* malformed Basic header — ignore */
    }
  }

  const resource = normalizeResource(ctx.identity, first("resource") ?? "");
  if (!resource) {
    return tokenError(
      ctx,
      "invalid_target",
      `resource must be ${ctx.identity.issuer} or ${ctx.identity.resource}`,
    );
  }
  const nowMs = ctx.nowMs();
  const nowSec = Math.floor(nowMs / 1000);
  const client = clientId ? await resolveClient(clientId, ctx.store, nowSec) : null;
  if (!clientId || !client) {
    return tokenError(ctx, "invalid_client", "unknown client_id");
  }

  // CIMD private_key_jwt (OpenAI): verify client_assertion against ChatGPT JWKS.
  const assertion = first("client_assertion");
  const assertionType = first("client_assertion_type");
  if (assertion) {
    if (assertionType && assertionType !== JWT_BEARER) {
      return tokenError(ctx, "invalid_client", "unsupported client_assertion_type");
    }
    if ((assertion.length > ctx.maxParamBytes || clientId.length > ctx.maxParamBytes)) {
      return tokenError(ctx, "invalid_client", "client_assertion too large");
    }
    if (!isChatgptOAuthClientId(clientId)) {
      return tokenError(ctx, "invalid_client", "client_assertion requires CIMD client_id");
    }
    const verdict = await verifyChatgptPrivateKeyJwt(
      assertion,
      clientId,
      ctx.identity.issuer,
      nowSec,
      ctx.fetchFn,
    );
    if (!verdict.ok) {
      return tokenError(ctx, "invalid_client", "client_assertion verification failed");
    }
  } else if (!(await authenticateClient(client, presentedSecret))) {
    return tokenError(ctx, "invalid_client", "client authentication failed");
  }

  if (grantType === "authorization_code") {
    const code = first("code");
    const redirectUri = first("redirect_uri");
    const codeVerifier = first("code_verifier");
    const scopeParam = first("scope");
    if (scopeParam && scopeParam !== OAUTH_SCOPE) {
      return tokenError(ctx, "invalid_scope", `unsupported scope '${scopeParam}'`);
    }
    if (!code) {
      return tokenError(ctx, "invalid_grant", "code is required");
    }
    const codeHash = await hashOpaqueToken(code);
    const consumed = await ctx.store.consumeCode(codeHash, nowMs); // one-use: consumed before validation
    if (!consumed.ok) {
      return tokenError(
        ctx,
        "invalid_grant",
        consumed.code === "expired" ? "authorization code expired" : "unknown or already-used authorization code",
      );
    }
    const entry = consumed.record;
    if (nowMs > entry.expires_at) {
      return tokenError(ctx, "invalid_grant", "authorization code expired");
    }
    if (!timingSafeStringEqual(entry.client_id, clientId)) {
      return tokenError(ctx, "invalid_grant", "code was issued to a different client");
    }
    if (!timingSafeStringEqual(entry.redirect_uri, redirectUri)) {
      return tokenError(ctx, "invalid_grant", "redirect_uri mismatch");
    }
    if (!timingSafeStringEqual(entry.resource, resource)) {
      return tokenError(ctx, "invalid_target", "resource mismatch");
    }
    if (!(await verifyPkceS256(codeVerifier, entry.code_challenge))) {
      return tokenError(ctx, "invalid_grant", "PKCE verification failed");
    }
    const pair = await ctx.store.issueTokens({
      client_id: clientId,
      resource,
      now_sec: nowSec,
      access_ttl_sec: ctx.accessTtlSec,
      refresh_ttl_sec: ctx.refreshTtlSec,
    });
    if (!pair) return tokenError(ctx, "server_error", "token issuance failed");
    return ctx.json(pair, 200, { "cache-control": "no-store" });
  }

  if (grantType === "refresh_token") {
    const rt = first("refresh_token");
    if (!rt) {
      return tokenError(ctx, "invalid_grant", "refresh_token is required");
    }
    const rtHash = await hashOpaqueToken(rt);
    // Atomic rotate + ownership check + issue inside the DO; old token is
    // rejected on replay. Failures collapse to the DO's generic invalid_grant.
    const pair = await ctx.store.exchangeRefresh({
      hash: rtHash,
      client_id: clientId,
      resource,
      now_sec: nowSec,
      access_ttl_sec: ctx.accessTtlSec,
      refresh_ttl_sec: ctx.refreshTtlSec,
    });
    if (!pair) {
      return tokenError(ctx, "invalid_grant", "unknown, expired or already-used refresh_token");
    }
    return ctx.json(pair, 200, { "cache-control": "no-store" });
  }

  return tokenError(ctx, "unsupported_grant_type", `unsupported grant_type '${grantType}'`);
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/**
 * Handle one public OAuth HTTP request. Returns a Response for paths this
 * handler owns (discovery, DCR aliases, /oauth/authorize, /oauth/token, plus
 * CORS OPTIONS on those paths) and `null` for everything else so a caller
 * (index.ts) can fall through to its own routing.
 */
export async function handleOAuthPublic(
  request: Request,
  options: OAuthPublicOptions,
): Promise<Response | null> {
  const url = new URL(request.url);
  const path = url.pathname;
  if (!isOwnedPath(path)) return null;

  const identity = options.identity;
  const store = options.store;
  const cors = corsHeaders();
  const maxBodyBytes = options.maxBodyBytes ?? DEFAULT_MAX_BODY_BYTES;
  const nowMs = options.nowMs ?? Date.now;
  const fetchFn = options.fetchFn ?? globalThis.fetch;
  const serverName = options.serverName ?? DEFAULT_SERVER_NAME;
  const serverVersion = options.serverVersion ?? DEFAULT_SERVER_VERSION;
  const accessTtlSec = options.accessTokenTtlSec ?? DEFAULT_ACCESS_TTL_S;
  const refreshTtlSec = options.refreshTokenTtlSec ?? DEFAULT_REFRESH_TTL_S;
  const codeTtlMs = options.codeTtlMs ?? DEFAULT_CODE_TTL_MS;
  const maxQueryBytes = options.maxQueryBytes ?? DEFAULT_MAX_QUERY_BYTES;
  const maxParamBytes = options.maxParamBytes ?? DEFAULT_MAX_PARAM_BYTES;
  const hctx: HandlerCtx = {
    identity,
    store,
    fetchFn,
    nowMs,
    accessTtlSec,
    refreshTtlSec,
    codeTtlMs,
    maxBodyBytes,
    maxQueryBytes,
    maxParamBytes,
    cors,
    json: (payload, status = 200, extra = {}) =>
      new Response(JSON.stringify(payload), {
        status,
        headers: { ...cors, "content-type": "application/json", ...extra },
      }),
  };

  // CORS preflight on any owned path.
  if (request.method === "OPTIONS") {
    return new Response(null, { status: 204, headers: cors });
  }

  if (request.method === "GET") {
    if (METADATA_PATHS.has(path)) {
      return hctx.json(oauthEdgeMetadata(identity));
    }
    if (PROTECTED_RESOURCE_PATHS.has(path)) {
      const which = PROTECTED_RESOURCE_PATHS.get(path)!;
      const resource = which === "issuer" ? identity.issuer : identity.resource;
      return hctx.json(protectedResourceEdgeMetadata(identity, resource));
    }
    if (path === MCP_JSON_PATH) {
      return hctx.json(mcpServerCardMetadata(identity, serverName, serverVersion));
    }
    if (path === AUTHORIZE_PATH) {
      return handleAuthorize(url, hctx);
    }
    return null;
  }

  if (request.method === "POST") {
    if (REGISTER_PATHS.has(path)) {
      return handleRegister(request, hctx);
    }
    if (path === TOKEN_PATH) {
      return handleToken(request, hctx);
    }
    return null;
  }

  return null;
}

// ---------------------------------------------------------------------------
// Store adapter over the OAUTH_STORE_DO internal HTTP API
// ---------------------------------------------------------------------------

/**
 * Adapter that drives the existing `OAUTH_STORE_DO` internal HTTP API. Pass a
 * DO stub (`env.OAUTH_STORE_DO.get(env.OAUTH_STORE_DO.idFromName("oauth-v1"))`)
 * or any object whose `.fetch` routes these paths. No secondary state.
 */
export function createOAuthPublicStore(stub: DoStub): OAuthPublicStore {
  const internal = async (path: string, body: unknown): Promise<Response> => {
    return stub.fetch(
      new Request(`https://oauth.internal${path}`, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify(body),
      }),
    );
  };

  const tokenPair = async (resp: Response): Promise<IssuedTokenPair | null> => {
    if (!resp.ok) return null;
    const data = (await resp.json()) as {
      ok?: boolean;
      token?: IssuedTokenPair & { key_id?: string };
    };
    if (!data.ok || !data.token) return null;
    const { key_id: _keyId, ...pair } = data.token; // strip DO-only key_id
    return pair;
  };

  return {
    async getClient(clientId) {
      const resp = await internal("/internal/oauth/client/get", { client_id: clientId });
      if (!resp.ok) return null;
      const data = (await resp.json()) as { ok?: boolean; record?: OAuthClientRecord };
      return data.record ?? null;
    },
    async putClient(clientId, record) {
      const resp = await internal("/internal/oauth/client/put", { client_id: clientId, record });
      return resp.ok;
    },
    async putCode(hash, record, nowMs) {
      const resp = await internal("/internal/oauth/code/put", { hash, record, now_ms: nowMs });
      return resp.ok;
    },
    async consumeCode(hash, nowMs) {
      const resp = await internal("/internal/oauth/code/consume", { hash, now_ms: nowMs });
      if (!resp.ok) {
        const data = (await resp.json().catch(() => null)) as { code?: string } | null;
        return { ok: false, code: data?.code === "expired" ? "expired" : "not_found" };
      }
      const data = (await resp.json()) as { ok?: boolean; record?: OAuthCodeRecord };
      return data.record ? { ok: true, record: data.record } : { ok: false, code: "not_found" };
    },
    async issueTokens(input) {
      const resp = await internal("/internal/oauth/token/issue", {
        client_id: input.client_id,
        resource: input.resource,
        now_sec: input.now_sec,
        access_ttl_sec: input.access_ttl_sec,
        refresh_ttl_sec: input.refresh_ttl_sec,
      });
      return tokenPair(resp);
    },
    async exchangeRefresh(input) {
      const resp = await internal("/internal/oauth/refresh/exchange", {
        hash: input.hash,
        client_id: input.client_id,
        resource: input.resource,
        now_sec: input.now_sec,
        access_ttl_sec: input.access_ttl_sec,
        refresh_ttl_sec: input.refresh_ttl_sec,
      });
      return tokenPair(resp);
    },
  };
}