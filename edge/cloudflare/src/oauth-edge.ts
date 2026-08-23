/**
 * oauth-edge.ts — dependency-light, dev OAuth compatibility primitives for
 * the Cloudflare edge (plan §7, Phase 4).
 *
 * Goal: model the CURRENT local runtime's OAuth metadata + token validation
 * semantics so a future cutover can validate existing ChatGPT-issued access
 * tokens WITHOUT forcing reauthorization. This module is PURE and TESTABLE:
 *
 *  - No filesystem, no env, no PEM reading, no live credential state.
 *  - No jose / node:crypto dependency — Web Crypto + base64url only, so it
 *    runs identically under Node tests and the Workers runtime.
 *  - Keying and client/refresh/token state are INJECTED interfaces. Nothing
 *    here embeds or reads a secret.
 *
 * It intentionally does NOT implement authorization/token endpoints or DCR
 * issuance (those belong to edge endpoints wired in a later phase). This
 * module mirrors, field-for-field, the semantics of `src/oauth.ts`:
 *
 *   - RFC 8414/OIDC-compatible authorization-server metadata (same keys /
 *     array order as `oauthMetadata()`).
 *   - RFC 9728 protected-resource metadata where `resource` is supplied
 *     explicitly (root vs `/mcp` document semantics preserved).
 *   - MCP server card (`/.well-known/mcp.json`).
 *   - RS256 compact-JWT access-token verification: exact issuer, aud = the
 *     canonical protected resource OR the issuer, exp <= now rejected,
 *     not-yet-valid nbf rejected; NO `typ` requirement (src/oauth.ts does not
 *     enforce one at verify time). `client_id` claim preferred, `sub` fallback.
 *   - Opaque-legacy access-token fallback hashed as
 *     SHA-256("herdr-mcp-oauth:" + token) against an injected store; expired
 *     records are deleted before returning failure.
 *
 * IMPORTANT (zero-reauthorization cutover): this module never invents an
 * issuer/resource. `createOAuthIdentity` REQUIRES the exact production issuer
 * be supplied — using a dev hostname as the issuer would reject
 * production-issued tokens and silently force reauthorization. That exactness
 * is a cutover decision that lives with the caller/deployment, not here.
 */

// ---------------------------------------------------------------------------
// Identity + resource normalization
// ---------------------------------------------------------------------------

export const OAUTH_SCOPE = "mcp";

export interface OAuthEdgeIdentity {
  /** RFC 8414 issuer — MUST be the exact production base URL (no trailing slash). */
  issuer: string;
  /** Canonical protected resource = `${issuer}/mcp`. */
  resource: string;
}

/**
 * Build the edge's OAuth identity from a required issuer. Trailing slashes
 * are stripped. `resource` is derived as `<issuer>/mcp` to match
 * `oauthResourceUrl()` in src/oauth.ts. There is deliberately NO default: an
 * empty/missing issuer fails closed (a wrong/dev issuer would break zero-
 * reauthorization cutover).
 */
export function createOAuthIdentity(issuer: string): OAuthEdgeIdentity {
  const trimmed = (issuer ?? "").trim().replace(/\/+$/, "");
  if (!trimmed) {
    throw new Error("oauth-edge: issuer is required; a dev/default host would break zero-reauthorization cutover");
  }
  return { issuer: trimmed, resource: `${trimmed}/mcp` };
}

/**
 * Resource normalization mirroring `normalizeResource` in src/oauth.ts:
 * missing/empty, the bare issuer (with/without trailing slash), or the
 * canonical resource URL (with/without trailing slash) all map to the
 * canonical protected resource; any foreign value returns null.
 */
export function normalizeResource(identity: OAuthEdgeIdentity, resource: string): string | null {
  if (resource === "") return identity.resource;
  const trimmed = resource.replace(/\/+$/, "");
  if (trimmed === identity.issuer || trimmed === `${identity.issuer}/mcp`) {
    return identity.resource;
  }
  return null;
}

// ---------------------------------------------------------------------------
// Metadata documents (deep-equivalent to src/oauth.ts `oauthMetadata()`)
// ---------------------------------------------------------------------------

/**
 * RFC 8414 authorization-server metadata + RFC 8414 §5 OIDC-discovery
 * compatibility. Same keys and array ordering as `oauthMetadata()`.
 */
export function oauthEdgeMetadata(identity: OAuthEdgeIdentity): Record<string, unknown> {
  const iss = identity.issuer;
  return {
    issuer: iss,
    authorization_endpoint: `${iss}/oauth/authorize`,
    token_endpoint: `${iss}/oauth/token`,
    registration_endpoint: `${iss}/oauth/register`,
    scopes_supported: [OAUTH_SCOPE],
    response_types_supported: ["code"],
    grant_types_supported: ["authorization_code", "refresh_token"],
    token_endpoint_auth_methods_supported: ["none", "private_key_jwt", "client_secret_post"],
    code_challenge_methods_supported: ["S256"],
    authorization_response_iss_parameter_supported: true, // RFC 9207 §4.2
    client_id_metadata_document_supported: true, // OpenAI ChatGPT CIMD
    protected_resources: [identity.resource], // RFC 9728 §4
  };
}

/**
 * RFC 9728 protected-resource metadata. `resource` is explicitly supplied by
 * the caller (preserving root vs `/token` document semantics: src/oauth.ts
 * serves `resource = issuer` at the root and `resource = issuer + /mcp` at the
 * path-aware forms).
 */
export function protectedResourceEdgeMetadata(
  identity: OAuthEdgeIdentity,
  resource: string,
): Record<string, unknown> {
  return {
    resource,
    authorization_servers: [identity.issuer],
    scopes_supported: [OAUTH_SCOPE],
    bearer_methods_supported: ["header"],
    resource_name: "herdr-mcp",
  };
}

/** `/.well-known/mcp.json` server card (MCP 2025-06-18+ requires serverUrl). */
export function mcpServerCardMetadata(
  identity: OAuthEdgeIdentity,
  name: string,
  version: string,
): Record<string, unknown> {
  return { serverUrl: identity.resource, name, version };
}

// ---------------------------------------------------------------------------
// Web Crypto primitives (zero-dep; Node + Workers compatible)
// ---------------------------------------------------------------------------

const enc = new TextEncoder();

function bytesToHex(b: Uint8Array): string {
  let h = "";
  for (let i = 0; i < b.length; i++) h += b[i].toString(16).padStart(2, "0");
  return h;
}

const B64URL_CHARS =
  "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/** Decode an unpadded base64url string to bytes (handles padding if present). */
export function base64urlDecode(s: string): Uint8Array {
  if (typeof s !== "string" || s.length === 0) throw new Error("empty base64url");
  const table = new Map<string, number>();
  for (let i = 0; i < B64URL_CHARS.length; i++) table.set(B64URL_CHARS[i], i);
  const out: number[] = [];
  let buffer = 0;
  let bits = 0;
  for (const ch of s) {
    if (ch === "=") break; // tolerate trailing padding
    const v = table.get(ch);
    if (v === undefined) throw new Error("invalid base64url character");
    buffer = (buffer << 6) | v;
    bits += 6;
    if (bits >= 8) {
      bits -= 8;
      out.push((buffer >> bits) & 0xff);
    }
  }
  return Uint8Array.from(out);
}

function bytesToText(b: Uint8Array): string {
  let s = "";
  for (let i = 0; i < b.length; i++) s += String.fromCharCode(b[i]);
  return s;
}

/**
 * Hash an opaque token exactly as src/oauth.ts does for storage/fallback:
 * SHA-256("herdr-mcp-oauth:" + token), lowercase hex.
 */
export async function hashOpaqueToken(token: string): Promise<string> {
  const digest = await crypto.subtle.digest("SHA-256", enc.encode(`herdr-mcp-oauth:${token}`));
  return bytesToHex(new Uint8Array(digest));
}

// ---------------------------------------------------------------------------
// JWT access-token verification (RS256, stateless)
// ---------------------------------------------------------------------------

export type JwtVerdict =
  | { ok: true; clientId?: string }
  /** Unverifiable (malformed / bad signature / bad alg / wrong issuer / bad JSON). */
  | { ok: false; kind: "unverifiable"; reason: string }
  /** Verified structurally but a required claim fails (aud/exp/nbf). */
  | { ok: false; kind: "rejected"; reason: string };

/**
 * Stateless access-token verifier. `verify` THROWS only for "unverifiable"
 * failures (which fall through to the opaque store, mirroring src/oauth.ts);
 * it returns `{ ok:false, kind:"rejected" }` for semantic claim rejections
 * (which in src/oauth.ts return without opaque fallback).
 */
export interface JwtAccessTokenVerifier {
  verify(token: string, nowSec: number): Promise<JwtVerdict>;
}

/**
 * Build an RS256 verifier bound to the exact edge identity (issuer/resource).
 * Signature is verified before any claim is accepted. Claims:
 *  - iss must equal identity.issuer exactly (else throws → unverifiable).
 *  - aud (string or array) must contain identity.resource OR identity.issuer
 *    (else rejected).
 *  - exp: present-but-not-number throws; `exp <= nowSec` rejected.
 *  - nbf: present-but-not-number throws; `nbf > nowSec` rejected.
 *  - client_id claim preferred; sub fallback.
 * `exp`/`nbf` are NOT required, matching src/oauth.ts (jose does not require
 * them). `typ: "at+jwt"` is NOT required (src/oauth.ts signs with it but never
 * enforces it at verify).
 */
export function createRs256AccessTokenVerifier(
  identity: OAuthEdgeIdentity,
  publicKey: CryptoKey,
): JwtAccessTokenVerifier {
  const verify = async (token: string, nowSec: number): Promise<JwtVerdict> => {
    const parts = token.split(".");
    if (parts.length !== 3) {
      throw new Error("malformed access token: expected compact JWT (3 segments)");
    }
    const [headerB64, payloadB64, signatureB64] = parts;

    let header: Record<string, unknown>;
    let payload: Record<string, unknown>;
    try {
      header = JSON.parse(bytesToText(base64urlDecode(headerB64))) as Record<string, unknown>;
      payload = JSON.parse(bytesToText(base64urlDecode(payloadB64))) as Record<string, unknown>;
    } catch {
      throw new Error("malformed access token: bad JSON/encoding");
    }

    if (header.alg !== "RS256") {
      throw new Error("unsupported access-token algorithm");
    }

    // Signature first (matches jose: verify before claims).
    const data = enc.encode(`${headerB64}.${payloadB64}`);
    const signature = base64urlDecode(signatureB64);
    const valid = await crypto.subtle.verify("RSASSA-PKCS1-v1_5", publicKey, signature, data);
    if (!valid) {
      throw new Error("invalid access-token signature");
    }

    // Issuer: exact match (src/oauth.ts passes { issuer } to jose → throws on mismatch).
    if (payload.iss !== identity.issuer) {
      throw new Error("access-token issuer mismatch");
    }

    // Audience: REQUIRED and computed unconditionally, exactly like
    // src/oauth.ts. Missing `aud` (undefined) or `aud: null` yields
    // `audOk === false` → semantic rejection with NO opaque fallback.
    const aud = payload.aud;
    const audOk = Array.isArray(aud)
      ? aud.includes(identity.resource) || aud.includes(identity.issuer)
      : aud === identity.resource || aud === identity.issuer;
    if (!audOk) return { ok: false, kind: "rejected", reason: "audience mismatch" };

    // exp (optional). Non-numeric → throw (jose rejects). exp <= now → rejected.
    if (payload.exp !== undefined && payload.exp !== null) {
      if (typeof payload.exp !== "number") throw new Error("malformed exp claim");
      if (payload.exp <= nowSec) {
        return { ok: false, kind: "rejected", reason: "token expired" };
      }
    }

    // nbf (optional). Non-numeric → throw; not-yet-valid (nbf > now) rejected.
    if (payload.nbf !== undefined && payload.nbf !== null) {
      if (typeof payload.nbf !== "number") throw new Error("malformed nbf claim");
      if (payload.nbf > nowSec) {
        return { ok: false, kind: "rejected", reason: "token not yet valid" };
      }
    }

    const clientId =
      (typeof payload.client_id === "string" && payload.client_id.length > 0 && payload.client_id) ||
      (typeof payload.sub === "string" && payload.sub.length > 0 && payload.sub) ||
      undefined;
    return clientId ? { ok: true, clientId } : { ok: true };
  };
  return { verify };
}

// ---------------------------------------------------------------------------
// Opaque legacy access-token store (injected)
// ---------------------------------------------------------------------------

export interface AccessTokenRecord {
  client_id: string;
  resource: string;
  scope: string;
  /** epoch seconds */
  expires_at: number;
}

export type RefreshTokenRecord = AccessTokenRecord;

export interface OpaqueAccessTokenStore {
  get(hash: string): AccessTokenRecord | undefined;
  delete(hash: string): void;
}

export interface AccessTokenInfo {
  ok: boolean;
  clientId?: string;
}

/**
 * Resolve a presented Bearer token mirroring `resolveAccessToken` in
 * src/oauth.ts:
 *  1. If the token contains "." and a JWT verifier is configured, try JWT.
 *     - unverifiable (verify throws) → fall through to the opaque store.
 *     - rejected (aud/exp/nbf) → return { ok:false } WITHOUT opaque fallback.
 *  2. Otherwise/opaque fallback: hash(token) → opaque store lookup.
 *     - missing → { ok:false }
 *     - nowSec > expires_at → delete + { ok:false }.
 *  Opaque expiry asymmetry preserved: opaque rejects only when now > expires_at,
 *  while the JWT path rejects on exp <= now (per src/oauth.ts).
 */
export async function resolveAccessToken(
  token: string,
  ctx: {
    identity: OAuthEdgeIdentity;
    verifier: JwtAccessTokenVerifier | null;
    opaqueStore: OpaqueAccessTokenStore;
    nowSec?: number;
  },
): Promise<AccessTokenInfo> {
  const nowSec = ctx.nowSec ?? Math.floor(Date.now() / 1000);

  if (token.includes(".") && ctx.verifier) {
    let verdict: JwtVerdict;
    try {
      verdict = await ctx.verifier.verify(token, nowSec);
    } catch {
      verdict = { ok: false, kind: "unverifiable", reason: "verification failed" };
    }
    if (verdict.ok) {
      return verdict.clientId ? { ok: true, clientId: verdict.clientId } : { ok: true };
    }
    // Structurally-valid JWT rejected on a claim → fail closed, no opaque fallback.
    if (verdict.kind === "rejected") return { ok: false };
    // Unverifiable → fall through to opaque legacy tokens below.
  }

  const hash = await hashOpaqueToken(token);
  const entry = ctx.opaqueStore.get(hash);
  if (!entry) return { ok: false };
  if (nowSec > entry.expires_at) {
    ctx.opaqueStore.delete(hash);
    return { ok: false };
  }
  return { ok: true, clientId: entry.client_id };
}

// ---------------------------------------------------------------------------
// State-shape normalization (models what src/oauth.ts imports on startup)
// ---------------------------------------------------------------------------

/**
 * Model the persisted `StoredClient` shape (src/oauth.ts `clients.json`).
 * `client_secret` is NEVER stored raw — only `client_secret_hash`. This is a
 * pure model, not a live registry; DCR clients still require importing the
 * persisted `clients.json` (they are NOT stateless).
 */
export interface StoredClient {
  client_secret_hash: string | null;
  redirect_uris: string[];
  token_endpoint_auth_method: "none" | "client_secret_post";
  grant_types: string[];
  scope: string;
  client_name?: string;
  issued_at: number;
}

/**
 * Normalize one parsed client record from `clients.json`, mirroring the
 * defaults/filtering in src/oauth.ts `loadAll()`. Returns null when the record
 * is structurally invalid (missing redirect_uris / empty id).
 */
export function normalizeStoredClient(
  clientId: string,
  raw: unknown,
  nowSec: number,
): StoredClient | null {
  if (!clientId) return null;
  const c = (raw ?? {}) as Partial<StoredClient>;
  if (!Array.isArray(c.redirect_uris)) return null;
  return {
    client_secret_hash: typeof c.client_secret_hash === "string" ? c.client_secret_hash : null,
    redirect_uris: c.redirect_uris.filter((u): u is string => typeof u === "string"),
    token_endpoint_auth_method:
      c.token_endpoint_auth_method === "client_secret_post" ? "client_secret_post" : "none",
    grant_types: Array.isArray(c.grant_types)
      ? c.grant_types.filter((g): g is string => typeof g === "string")
      : ["authorization_code", "refresh_token"],
    scope: typeof c.scope === "string" && c.scope.length > 0 ? c.scope : OAUTH_SCOPE,
    ...(typeof c.client_name === "string" ? { client_name: c.client_name } : {}),
    issued_at: typeof c.issued_at === "number" ? c.issued_at : nowSec,
  };
}

/**
 * Normalize one parsed opaque token record (from `tokens.json` or
 * `refresh.json`) with the same filtering as src/oauth.ts `loadAll()`:
 * invalid records and records already expired (`expires_at <= nowSec`) are
 * dropped; `resource` defaults to the canonical resource when absent.
 */
export function normalizeStoredToken(
  raw: unknown,
  nowSec: number,
  identity: OAuthEdgeIdentity,
): AccessTokenRecord | null {
  const t = (raw ?? {}) as Partial<AccessTokenRecord>;
  if (typeof t.client_id !== "string" || typeof t.expires_at !== "number" || t.expires_at <= nowSec) {
    return null;
  }
  return {
    client_id: t.client_id,
    resource: typeof t.resource === "string" ? t.resource : identity.resource,
    scope: typeof t.scope === "string" ? t.scope : OAUTH_SCOPE,
    expires_at: t.expires_at,
  };
}
