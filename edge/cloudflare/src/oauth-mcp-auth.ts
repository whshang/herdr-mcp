import { authenticateStaticMcpBearer } from "./auth.js";
import {
  createOAuthIdentity,
  createRs256AccessTokenVerifier,
  type AccessTokenInfo,
} from "./oauth-edge.js";

export interface OAuthMcpAuthEnv {
  DEV_MCP_BEARER_SECRET?: string;
  STATIC_MCP_BEARER_SECRET?: string;
  OAUTH_ISSUER?: string;
  OAUTH_JWT_PUBLIC_PEM?: string;
}

export type McpAuthResult =
  | { ok: true; source: "dev_bearer" | "static_bearer" | "oauth_jwt" | "oauth_edge"; clientId?: string }
  | { ok: false; code: "mcp_auth_failed" };

export interface OAuthMcpAuthDeps {
  verifyEdgeToken?: (token: string) => Promise<{ ok: boolean; clientId?: string }>;
}

let cachedPublicPem = "";
let cachedPublicKey: CryptoKey | undefined;

function presentedBearer(request: Request): string | null {
  const raw = request.headers.get("authorization") ?? "";
  const match = /^Bearer\s+([^\s]+)$/i.exec(raw.trim());
  return match ? match[1] : null;
}

function pemDer(pem: string, label: string): Uint8Array {
  const begin = `-----BEGIN ${label}-----`;
  const end = `-----END ${label}-----`;
  const start = pem.indexOf(begin);
  const stop = pem.indexOf(end);
  if (start < 0 || stop <= start) throw new Error(`oauth-mcp-auth: invalid ${label} PEM`);
  const b64 = pem.slice(start + begin.length, stop).replace(/\s+/g, "");
  if (!b64) throw new Error(`oauth-mcp-auth: empty ${label} PEM`);
  const alphabet = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
  const out: number[] = [];
  let buffer = 0;
  let bits = 0;
  for (const ch of b64.replace(/=+$/, "")) {
    const value = alphabet.indexOf(ch);
    if (value < 0) throw new Error(`oauth-mcp-auth: invalid ${label} base64`);
    buffer = (buffer << 6) | value;
    bits += 6;
    if (bits >= 8) {
      bits -= 8;
      out.push((buffer >> bits) & 0xff);
    }
  }
  return Uint8Array.from(out);
}

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

async function publicKeyFor(pem: string): Promise<CryptoKey> {
  if (cachedPublicKey && cachedPublicPem === pem) return cachedPublicKey;
  const key = await importRs256PublicKeyPem(pem);
  cachedPublicPem = pem;
  cachedPublicKey = key;
  return key;
}

/**
 * Phase-4 transition auth for /mcp.
 *
 * - Temporary dev bearer remains accepted for operator smoke tests.
 * - A production-issuer RS256 JWT is accepted using the migrated public key.
 * - Opaque legacy access tokens are intentionally not accepted here yet; the
 *   OAuthStateDO integration adds that fallback before Phase 5.
 * - Missing OAuth config fails closed for JWTs and never weakens the dev gate.
 */
export async function authenticateMcpRequest(
  request: Request,
  env: OAuthMcpAuthEnv,
  deps: OAuthMcpAuthDeps = {},
): Promise<McpAuthResult> {
  const dev = authenticateStaticMcpBearer(request, env.DEV_MCP_BEARER_SECRET);
  if (dev.ok) return { ok: true, source: "dev_bearer" };

  const staticBearer = authenticateStaticMcpBearer(request, env.STATIC_MCP_BEARER_SECRET);
  if (staticBearer.ok) return { ok: true, source: "static_bearer" };

  const token = presentedBearer(request);
  if (!token) return { ok: false, code: "mcp_auth_failed" };
  const issuer = env.OAUTH_ISSUER?.trim();
  const pem = env.OAUTH_JWT_PUBLIC_PEM;
  if (token.includes(".") && issuer && pem) {
    try {
      const identity = createOAuthIdentity(issuer);
      const verifier = createRs256AccessTokenVerifier(identity, await publicKeyFor(pem));
      const verdict = await verifier.verify(token, Math.floor(Date.now() / 1000));
      if (verdict.ok) {
        const result: AccessTokenInfo = verdict.clientId
          ? { ok: true, clientId: verdict.clientId }
          : { ok: true };
        return result.clientId
          ? { ok: true, source: "oauth_jwt", clientId: result.clientId }
          : { ok: true, source: "oauth_jwt" };
      }
    } catch {
      // A token not signed by the legacy key may be an Edge-issued JWT.
    }
  }

  if (deps.verifyEdgeToken) {
    try {
      const edge = await deps.verifyEdgeToken(token);
      if (edge.ok) {
        return edge.clientId
          ? { ok: true, source: "oauth_edge", clientId: edge.clientId }
          : { ok: true, source: "oauth_edge" };
      }
    } catch {
      // Fail closed below.
    }
  }
  return { ok: false, code: "mcp_auth_failed" };
}
