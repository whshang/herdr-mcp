/**
 * oauth-token-crypto.ts — pure WebCrypto helpers for herdr-mcp Phase 4 OAuth.
 *
 * COMPLETELY SELF-CONTAINED: no node:crypto, no jose, no filesystem, no env.
 * Every function runs identically under Node 24+ (global crypto) and
 * Cloudflare Workers.
 *
 * Design boundaries:
 *  - Importing keys: PEM (PKCS8 private, SPKI public) → CryptoKey, zero deps.
 *  - Issuing RS256 compact access JWTs (src/oauth.ts claims/header match).
 *  - Verifying ChatGPT CIMD private_key_jwt assertions with injected fetch.
 *  - PKCE S256 code_verifier verification (constant-time).
 *  - Random base64url token generation (crypto.getRandomValues).
 *
 * NOT in scope:
 *  - OAuth endpoint routing, DCR, client/refresh/code store, opaque tokens.
 *  - PEM serialization (not needed for edge/Workers).
 *  - Live network calls (fetch is injected).
 */
import { base64urlDecode } from "./oauth-edge.js";

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

export const PKCE_VERIFIER_RE = /^[A-Za-z0-9\-._~]{43,128}$/;

export const RS256_HEADER = Object.freeze({ alg: "RS256", typ: "at+jwt" });

const B64_ALPHABET = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

// ---------------------------------------------------------------------------
// Base64url helpers
// ---------------------------------------------------------------------------

const enc = new TextEncoder();

function base64urlEncode(bytes: Uint8Array): string {
  let s = "";
  for (const b of bytes) s += String.fromCharCode(b);
  return btoa(s).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

function bytesToText(bytes: Uint8Array): string {
  let s = "";
  for (let i = 0; i < bytes.length; i++) s += String.fromCharCode(bytes[i]);
  return s;
}

// ---------------------------------------------------------------------------
// PEM → DER (no-op for base64-only; PEM is base64 + header/footer)
// ---------------------------------------------------------------------------

/**
 * Strip PEM header/footer/whitespace and decode the base64 body to DER bytes.
 * Only the canonical PEM header/footer boundaries are recognized.
 */
function pemDer(pem: string, label: string): Uint8Array {
  const begin = `-----BEGIN ${label}-----`;
  const end = `-----END ${label}-----`;
  const start = pem.indexOf(begin);
  const stop = pem.indexOf(end);
  if (start < 0 || stop <= start) throw new Error(`invalid ${label} PEM: missing header/footer`);
  const b64 = pem.slice(start + begin.length, stop).replace(/\s+/g, "");
  if (!b64) throw new Error(`invalid ${label} PEM: empty body`);
  // Decode standard base64 to bytes
  const out: number[] = [];
  let buffer = 0;
  let bits = 0;
  for (const ch of b64.replace(/=+$/, "")) {
    const value = B64_ALPHABET.indexOf(ch);
    if (value < 0) throw new Error(`invalid ${label} PEM: unexpected base64 character`);
    buffer = (buffer << 6) | value;
    bits += 6;
    if (bits >= 8) {
      bits -= 8;
      out.push((buffer >> bits) & 0xff);
    }
  }
  return Uint8Array.from(out);
}

// ---------------------------------------------------------------------------
// 1. Key import (PKCS8 private, SPKI public) — pure Web Crypto, no node:crypto
// ---------------------------------------------------------------------------

/**
 * Import an RS256 private key from a PKCS#8 PEM string.
 * @param pem — PEM string with "-----BEGIN PRIVATE KEY-----" header
 */
export async function importRs256PrivateKeyPem(pem: string): Promise<CryptoKey> {
  const der = pemDer(pem, "PRIVATE KEY");
  return crypto.subtle.importKey(
    "pkcs8",
    der,
    { name: "RSASSA-PKCS1-v1_5", hash: "SHA-256" },
    false,
    ["sign"],
  );
}

/**
 * Import an RS256 public key from an SPKI PEM string.
 * @param pem — PEM string with "-----BEGIN PUBLIC KEY-----" header
 */
export async function importRs256PublicKeyPem(pem: string): Promise<CryptoKey> {
  const der = pemDer(pem, "PUBLIC KEY");
  return crypto.subtle.importKey(
    "spki",
    der,
    { name: "RSASSA-PKCS1-v1_5", hash: "SHA-256" },
    false,
    ["verify"],
  );
}

// ---------------------------------------------------------------------------
// 2. Issue an RS256 compact access JWT (mirrors src/oauth.ts issueTokens)
// ---------------------------------------------------------------------------

/**
 * Create an RS256-signed compact JWT access token matching the exact claims
 * and header shape that src/oauth.ts `issueTokens` produces.
 *
 * Header:  { alg: "RS256", typ: "at+jwt" }
 * Claims:
 *   - client_id: the supplied clientId
 *   - scope: "mcp"
 *   - iss: issuer (exact, no trailing slash)
 *   - aud: resource (the canonical protected resource URL)
 *   - sub: clientId
 *   - jti: supplied or auto-generated random hex
 *   - iat: now
 *   - exp: now + ttlSeconds
 *   - (any additionalClaims are merged in)
 *
 * Exported separately so tests can create tokens without the private key
 * store (real tokens are issued by src/oauth.ts or the edge token endpoint).
 */
export async function issueRs256AccessJwt(
  privateKey: CryptoKey,
  issuer: string,
  resource: string,
  clientId: string,
  ttlSeconds: number,
  options?: {
    jti?: string;
    nowSec?: number;
    additionalClaims?: Record<string, unknown>;
  },
): Promise<string> {
  const now = options?.nowSec ?? Math.floor(Date.now() / 1000);
  const jti = options?.jti ?? randomHex(16);
  const payload: Record<string, unknown> = {
    client_id: clientId,
    scope: "mcp",
    iss: issuer,
    aud: resource,
    sub: clientId,
    jti,
    iat: now,
    exp: now + ttlSeconds,
    ...(options?.additionalClaims ?? {}),
  };
  const headerB64 = base64urlEncode(enc.encode(JSON.stringify(RS256_HEADER)));
  const payloadB64 = base64urlEncode(enc.encode(JSON.stringify(payload)));
  const data = enc.encode(`${headerB64}.${payloadB64}`);
  const sig = new Uint8Array(await crypto.subtle.sign("RSASSA-PKCS1-v1_5", privateKey, data));
  return `${headerB64}.${payloadB64}.${base64urlEncode(sig)}`;
}

function randomHex(byteLen: number): string {
  const buf = new Uint8Array(byteLen);
  crypto.getRandomValues(buf);
  let hex = "";
  for (const b of buf) hex += b.toString(16).padStart(2, "0");
  return hex;
}

// ---------------------------------------------------------------------------
// 3. Verify ChatGPT CIMD private_key_jwt assertion
// ---------------------------------------------------------------------------

/**
 * Detailed result of a client_assertion (private_key_jwt) verification.
 */
/** Discriminated verdict: `ok:true` carries clientId, `ok:false` carries a machine-readable code. */
export type AssertionVerdict =
  | { ok: true; clientId: string }
  | {
      ok: false;
      /** Machine-readable error code (never echoes assertion/key data). */
      code:
        | "bad_kid"
        | "bad_signature"
        | "bad_alg"
        | "bad_issuer"
        | "bad_audience"
        | "bad_sub"
        | "bad_jwks"
        | "bad_jwks_fetch"
        | "bad_jwks_size"
        | "bad_jwks_count"
        | "bad_jwks_format"
        | "expired"
        | "not_yet_valid"
        | "malformed";
    };

/**
 * Verify a ChatGPT CIMD private_key_jwt client_assertion with an injected
 * fetch function — no network calls happen unless the adapter calls fetch.
 *
 * Semantics mirror src/oauth.ts handleToken:
 *   - clientId must be an HTTPS URL; host must be chatgpt.com or www.chatgpt.com.
 *   - JWKS fetched from `${clientId origin}/oauth/jwks.json`.
 *   - JWK selected by kid when present in assertion header; first RS256 key fallback.
 *   - Only RS256 keys accepted; other key types → error.
 *   - Signature verified with Web Crypto (RSASSA-PKCS1-v1_5 / SHA-256).
 *   - Issuer must equal clientId exactly.
 *   - aud must equal any of: `${oauthIssuer}/oauth/token`, oauthIssuer.
 *   - sub if present must equal clientId.
 *   - exp and nbf time checks (exp <= now, nbf > now).
 *   - Bounded JWKS response: 65536 bytes max, 16 keys max.
 *   - Never echoes assertion or key material in errors.
 *
 * @param assertion — compact JWT (3-segment base64url)
 * @param clientId — the CIMD client_id (HTTPS URL)
 * @param oauthIssuer — the authorization server issuer URL
 * @param nowSec — current epoch seconds for time checks
 * @param fetchFn — injected fetch implementation (e.g. globalThis.fetch)
 */
export async function verifyChatgptPrivateKeyJwt(
  assertion: string,
  clientId: string,
  oauthIssuer: string,
  nowSec: number,
  fetchFn: typeof globalThis.fetch,
): Promise<AssertionVerdict> {
  // 1. Parse the compact JWT header
  const parts = assertion.split(".");
  if (parts.length !== 3) return { ok: false, code: "malformed" };

  let header: Record<string, unknown>;
  let payload: Record<string, unknown>;
  let assertionKid: string | undefined;
  try {
    header = JSON.parse(bytesToText(base64urlDecode(parts[0]))) as Record<string, unknown>;
    payload = JSON.parse(bytesToText(base64urlDecode(parts[1]))) as Record<string, unknown>;
    if (typeof header.kid === "string" && header.kid.length > 0) {
      assertionKid = header.kid;
    }
  } catch {
    return { ok: false, code: "malformed" };
  }

  // 2. Alg must be RS256
  if (header.alg !== "RS256") return { ok: false, code: "bad_alg" };

  // 3. Validate clientId — must be HTTPS chatgpt.com
  let jwksOrigin: string;
  try {
    const u = new URL(clientId);
    if (u.protocol !== "https:") return { ok: false, code: "malformed" };
    const host = u.hostname.toLowerCase();
    if (host !== "chatgpt.com" && host !== "www.chatgpt.com") return { ok: false, code: "malformed" };
    jwksOrigin = u.origin;
  } catch {
    return { ok: false, code: "malformed" };
  }

  // 4. Fetch JWKS from `${origin}/oauth/jwks.json`
  let jwksResponse: Response;
  try {
    jwksResponse = await fetchFn(new URL("/oauth/jwks.json", jwksOrigin).href);
  } catch {
    return { ok: false, code: "bad_jwks_fetch" };
  }
  if (!jwksResponse.ok) return { ok: false, code: "bad_jwks_fetch" };

  // 5. Bounded response body (64 KiB)
  let jwksBody: ArrayBuffer;
  try {
    jwksBody = await jwksResponse.arrayBuffer();
  } catch {
    return { ok: false, code: "bad_jwks_fetch" };
  }
  if (jwksBody.byteLength > 65536) return { ok: false, code: "bad_jwks_size" };

  let jwks: { keys?: unknown[] };
  try {
    jwks = JSON.parse(new TextDecoder().decode(jwksBody)) as { keys?: unknown[] };
  } catch {
    return { ok: false, code: "bad_jwks_format" };
  }
  if (!Array.isArray(jwks.keys) || jwks.keys.length === 0) return { ok: false, code: "bad_jwks_format" };
  if (jwks.keys.length > 16) return { ok: false, code: "bad_jwks_count" };

  // 6. Select JWK by kid (present) or first RS256 key
  const jwk = findRs256Jwk(jwks.keys, assertionKid);
  if (!jwk) return { ok: false, code: assertionKid ? "bad_kid" : "bad_jwks_format" };

  // 7. Import JWK as CryptoKey
  let publicKey: CryptoKey;
  try {
    publicKey = await crypto.subtle.importKey(
      "jwk",
      jwk as unknown as JsonWebKey,
      { name: "RSASSA-PKCS1-v1_5", hash: "SHA-256" },
      false,
      ["verify"],
    );
  } catch {
    return { ok: false, code: "bad_jwks_format" };
  }

  // 8. Verify signature
  const data = enc.encode(`${parts[0]}.${parts[1]}`);
  let sigBytes: Uint8Array;
  try {
    sigBytes = base64urlDecode(parts[2]);
  } catch {
    return { ok: false, code: "malformed" };
  }
  const valid = await crypto.subtle.verify("RSASSA-PKCS1-v1_5", publicKey, sigBytes, data);
  if (!valid) return { ok: false, code: "bad_signature" };

  // 9. Claims: iss must equal clientId exactly
  if (payload.iss !== clientId) return { ok: false, code: "bad_issuer" };

  // 10. aud: must be a string or array containing `${oauthIssuer}/oauth/token` or oauthIssuer
  const aud = payload.aud;
  const tokenUrl = `${oauthIssuer}/oauth/token`;
  const audOk = Array.isArray(aud)
    ? aud.includes(tokenUrl) || aud.includes(oauthIssuer)
    : aud === tokenUrl || aud === oauthIssuer;
  if (!audOk) return { ok: false, code: "bad_audience" };

  // 11. sub: if present, must equal clientId
  if (payload.sub !== undefined && payload.sub !== null) {
    if (typeof payload.sub !== "string" || payload.sub !== clientId) {
      return { ok: false, code: "bad_sub" };
    }
  }

  // 12. exp time check
  if (payload.exp !== undefined && payload.exp !== null) {
    if (typeof payload.exp !== "number") return { ok: false, code: "malformed" };
    if (payload.exp <= nowSec) return { ok: false, code: "expired" };
  }

  // 13. nbf time check
  if (payload.nbf !== undefined && payload.nbf !== null) {
    if (typeof payload.nbf !== "number") return { ok: false, code: "malformed" };
    if (payload.nbf > nowSec) return { ok: false, code: "not_yet_valid" };
  }

  return { ok: true, clientId };
}

/**
 * Find an RS256 JWK from a JWKS key array, mirroring jose's getKey: a key
 * qualifies when its kty is RSA and its alg (when present) is RS256 — a key
 * with no `alg` is still eligible (jose matches by kid + kty, not a strict
 * alg equality). When kid is supplied, match by kid among eligible keys;
 * otherwise return the first eligible key.
 */
function findRs256Jwk(keys: unknown[], kid?: string): Record<string, unknown> | undefined {
  const rs256Keys = keys.filter((k): k is Record<string, unknown> => {
    if (k === null || typeof k !== "object") return false;
    const jwk = k as Record<string, unknown>;
    if (jwk.kty !== "RSA") return false;
    return jwk.alg === undefined || jwk.alg === "RS256";
  });
  if (rs256Keys.length === 0) return undefined;
  if (kid) {
    return rs256Keys.find((k) => k.kid === kid);
  }
  return rs256Keys[0];
}

// ---------------------------------------------------------------------------
// 4. PKCE S256 helpers
// ---------------------------------------------------------------------------

/**
 * Compute the S256 code challenge = BASE64URL(SHA256(ASCII(code_verifier))).
 * RFC 7636 §4.6.
 */
export async function s256Challenge(codeVerifier: string): Promise<string> {
  if (!PKCE_VERIFIER_RE.test(codeVerifier)) {
    throw new Error("PKCE code_verifier must match 43-128 unreserved characters");
  }
  const digest = await crypto.subtle.digest("SHA-256", enc.encode(codeVerifier));
  return base64urlEncode(new Uint8Array(digest));
}

/**
 * Constant-time PKCE S256 verification: compare computed challenge with
 * the supplied code_challenge using timing-safe byte comparison.
 */
export async function verifyPkceS256(
  codeVerifier: string,
  codeChallenge: string,
): Promise<boolean> {
  if (!PKCE_VERIFIER_RE.test(codeVerifier)) return false;
  const computed = await s256Challenge(codeVerifier);
  return timingSafeStringEqual(computed, codeChallenge);
}

/**
 * Timing-safe string comparison. Leaks only the length of the shorter
 * string (which is bounded by the base64url challenge length).
 */
function timingSafeStringEqual(a: string, b: string): boolean {
  const aBytes = enc.encode(a);
  const bBytes = enc.encode(b);
  if (aBytes.length !== bBytes.length) return false;
  let diff = 0;
  for (let i = 0; i < aBytes.length; i++) {
    diff |= aBytes[i] ^ bBytes[i];
  }
  return diff === 0;
}

// ---------------------------------------------------------------------------
// 5. Token generation helpers
// ---------------------------------------------------------------------------

/**
 * Generate a cryptographically random base64url token (32 bytes → 43 chars).
 * Uses crypto.getRandomValues (no node:crypto.randomBytes).
 */
export function randomBase64UrlToken(): string {
  const buf = new Uint8Array(32);
  crypto.getRandomValues(buf);
  return base64urlEncode(buf);
}

/**
 * Check if a client_id is a ChatGPT CIMD client_id (HTTPS URL whose host
 * is chatgpt.com or www.chatgpt.com). Mirrors src/oauth.ts `isChatgptOAuthClientId`.
 */
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