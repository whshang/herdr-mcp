/**
 * auth.ts — workstation-link authentication interface (dev implementation).
 *
 * Boundary ownership:
 *  - The Worker authenticates the WS *upgrade* before routing to the DO.
 *  - The DO additionally validates the link identity inside the `hello`
 *    message (workstationId binding + protocol version).
 *  - Link credentials are strictly separate from ChatGPT OAuth credentials
 *    (plan §12). OAuth itself is Phase 4 and intentionally NOT here.
 *
 * Dev-only: a single shared secret read from LINK_SHARED_SECRET (fail closed
 * when unset). Production will use per-workstation derived credentials with
 * rotation — see README TODO list. Nothing here is production-grade.
 */

import { MAX_WS_ID_LEN, MAX_RESOURCE_TOKEN_LEN } from "./limits.js";

export interface AuthClaims {
  workstationId: string;
  principal: "link";
  secretVersion: string;
  authedAtMs: number;
}

export type AuthDecision =
  | { ok: true; claims: AuthClaims }
  | { ok: false; code: "link_auth_failed" | "bad_request"; reason: string };

export type DevMcpAuthDecision =
  | { ok: true }
  | { ok: false; code: "mcp_auth_failed"; reason: string };

/** Minimal request view so auth stays pure and testable outside Workers. */
export interface AuthRequestLike {
  headers: { get(name: string): string | null };
}

export interface LinkAuthenticator {
  authenticate(request: AuthRequestLike, workstationId: string, atMs: number): AuthDecision | Promise<AuthDecision>;
}

/** Constant-time byte compare (dev-grade; length leak acceptable). */
export function constantTimeEqual(a: Uint8Array, b: Uint8Array): boolean {
  if (a.length !== b.length) return false;
  let diff = 0;
  for (let i = 0; i < a.length; i++) diff |= a[i] ^ b[i];
  return diff === 0;
}

const enc = new TextEncoder();

export const LINK_AUTH_PROTOCOL_PREFIX = "herdr-auth.";
export const LINK_APPLICATION_PROTOCOL = "herdr-link.v1";

export function requestedWebSocketProtocols(request: AuthRequestLike): string[] {
  const raw = request.headers.get("sec-websocket-protocol");
  return raw ? raw.split(",").map((part) => part.trim()).filter(Boolean) : [];
}

export function hasLinkApplicationProtocol(request: AuthRequestLike): boolean {
  return requestedWebSocketProtocols(request).includes(LINK_APPLICATION_PROTOCOL);
}

/**
 * Temporary Phase-3 ChatGPT-facing bearer gate for the public dev Worker.
 * OAuth replaces this in Phase 4. It is intentionally separate from the
 * workstation-link secret and fails closed when unset.
 */
export function authenticateDevMcpBearer(
  request: AuthRequestLike,
  secret: string | undefined,
): DevMcpAuthDecision {
  if (!secret || secret.length === 0) {
    return { ok: false, code: "mcp_auth_failed", reason: "dev MCP auth not configured; failing closed" };
  }
  const header = request.headers.get("authorization");
  if (!header) return { ok: false, code: "mcp_auth_failed", reason: "missing Authorization" };
  const match = /^Bearer\s+([A-Za-z0-9._~+/=-]+)$/.exec(header.trim());
  if (!match) return { ok: false, code: "mcp_auth_failed", reason: "unsupported Authorization scheme" };
  const token = match[1];
  if (token.length > MAX_RESOURCE_TOKEN_LEN) {
    return { ok: false, code: "mcp_auth_failed", reason: "credential rejected (length)" };
  }
  if (!constantTimeEqual(enc.encode(token), enc.encode(secret))) {
    return { ok: false, code: "mcp_auth_failed", reason: "credential rejected" };
  }
  return { ok: true };
}

/** Encode an arbitrary UTF-8 secret into the token-safe WS auth protocol. */
export function buildLinkAuthProtocol(secret: string): string {
  const bytes = enc.encode(secret);
  let hex = "";
  for (const byte of bytes) hex += byte.toString(16).padStart(2, "0");
  return `${LINK_AUTH_PROTOCOL_PREFIX}${hex}`;
}

/**
 * Dev shared-secret authenticator. Accepts either:
 * - `Authorization: Bearer <secret>` for non-WebSocket/manual clients; or
 * - `Sec-WebSocket-Protocol: ..., herdr-auth.<utf8-hex>` for the standard
 *   WHATWG WebSocket client, which cannot set an Authorization header.
 *
 * Fail closed when configured secret is missing/empty.
 */
export class DevSecretLinkAuthenticator implements LinkAuthenticator {
  private readonly secret: string | undefined;

  constructor(config: { secret?: string; secretVersion?: string }) {
    this.secret = config.secret && config.secret.length > 0 ? config.secret : undefined;
    this.secretVersion = config.secretVersion?.length ? config.secretVersion : "dev";
  }

  private readonly secretVersion: string;

  authenticate(request: AuthRequestLike, workstationId: string, atMs: number): AuthDecision {
    if (this.secret === undefined) {
      return { ok: false, code: "link_auth_failed", reason: "link auth not configured (dev secret missing); failing closed" };
    }
    if (!workstationId || workstationId.length === 0 || workstationId.length > MAX_WS_ID_LEN) {
      return { ok: false, code: "bad_request", reason: "invalid workstation_id" };
    }
    const header = request.headers.get("authorization");
    let credentialMatched = false;
    if (header) {
      const match = /^Bearer\s+([A-Za-z0-9._~+/=-]+)$/.exec(header.trim());
      if (!match) return { ok: false, code: "link_auth_failed", reason: "unsupported Authorization scheme" };
      const token = match[1];
      if (token.length > MAX_RESOURCE_TOKEN_LEN) {
        return { ok: false, code: "link_auth_failed", reason: "credential rejected (length)" };
      }
      credentialMatched = constantTimeEqual(enc.encode(token), enc.encode(this.secret));
    } else {
      const protocols = requestedWebSocketProtocols(request);
      if (protocols.length > 0) {
        const expected = buildLinkAuthProtocol(this.secret);
        credentialMatched = protocols.some((candidate) =>
          constantTimeEqual(enc.encode(candidate), enc.encode(expected)),
        );
      }
    }
    if (!credentialMatched) {
      return { ok: false, code: "link_auth_failed", reason: "credential rejected" };
    }
    return {
      ok: true,
      claims: { workstationId, principal: "link", secretVersion: this.secretVersion, authedAtMs: atMs },
    };
  }
}