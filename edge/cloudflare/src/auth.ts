/**
 * auth.ts — workstation-link and static bearer authentication primitives.
 *
 * Boundary ownership:
 *  - The Worker authenticates the WS *upgrade* before routing to the DO.
 *  - The DO additionally validates the link identity inside the `hello`
 *    message (workstationId binding + protocol version).
 *  - Link credentials are strictly separate from ChatGPT OAuth credentials.
 *
 * The current link uses a shared secret from LINK_SHARED_SECRET and fails
 * closed when unset. Per-workstation credential rotation remains a separate
 * hardening path; OAuth is already implemented elsewhere for MCP clients.
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

export type StaticMcpBearerDecision =
  | { ok: true }
  | { ok: false; code: "mcp_auth_failed"; reason: string };

export type LinkCredentialDecision =
  | { ok: true; credential: string; transport: "authorization" | "websocket_protocol" }
  | { ok: false; code: "link_auth_failed"; reason: string };

/** Minimal request view so auth stays pure and testable outside Workers. */
export interface AuthRequestLike {
  headers: { get(name: string): string | null };
}

export interface LinkAuthenticator {
  authenticate(request: AuthRequestLike, workstationId: string, atMs: number): AuthDecision | Promise<AuthDecision>;
}

/** Constant-time byte compare for equal-length credentials. */
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
 * Static bearer fallback used for controlled compatibility/admin paths. Public
 * ChatGPT access normally authenticates through OAuth. This secret is separate
 * from the workstation-link secret and fails closed when unset.
 */
export function authenticateStaticMcpBearer(
  request: AuthRequestLike,
  secret: string | undefined,
): StaticMcpBearerDecision {
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

function decodeLinkAuthProtocol(value: string): string | null {
  if (!value.startsWith(LINK_AUTH_PROTOCOL_PREFIX)) return null;
  const hex = value.slice(LINK_AUTH_PROTOCOL_PREFIX.length);
  if (hex.length === 0 || hex.length % 2 !== 0 || !/^[0-9a-f]+$/i.test(hex)) return null;
  if (hex.length > MAX_RESOURCE_TOKEN_LEN * 2) return null;
  const bytes = new Uint8Array(hex.length / 2);
  for (let i = 0; i < bytes.length; i++) bytes[i] = Number.parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  try {
    return new TextDecoder("utf-8", { fatal: true, ignoreBOM: false }).decode(bytes);
  } catch {
    return null;
  }
}

/** Extract one raw Link credential without deciding which device it belongs to. */
export function extractLinkCredential(request: AuthRequestLike): LinkCredentialDecision {
  const header = request.headers.get("authorization");
  if (header) {
    const match = /^Bearer\s+([A-Za-z0-9._~+/=-]+)$/.exec(header.trim());
    if (!match) return { ok: false, code: "link_auth_failed", reason: "unsupported Authorization scheme" };
    const credential = match[1];
    if (credential.length > MAX_RESOURCE_TOKEN_LEN) {
      return { ok: false, code: "link_auth_failed", reason: "credential rejected (length)" };
    }
    return { ok: true, credential, transport: "authorization" };
  }

  const authProtocols = requestedWebSocketProtocols(request).filter((value) => value.startsWith(LINK_AUTH_PROTOCOL_PREFIX));
  if (authProtocols.length !== 1) {
    return {
      ok: false,
      code: "link_auth_failed",
      reason: authProtocols.length > 1 ? "multiple websocket credentials are not allowed" : "missing link credential",
    };
  }
  const credential = decodeLinkAuthProtocol(authProtocols[0]);
  if (credential === null || credential.length > MAX_RESOURCE_TOKEN_LEN) {
    return { ok: false, code: "link_auth_failed", reason: "credential rejected" };
  }
  return { ok: true, credential, transport: "websocket_protocol" };
}

/**
 * Dev shared-secret authenticator. Accepts either:
 * - `Authorization: Bearer <secret>` for non-WebSocket/manual clients; or
 * - `Sec-WebSocket-Protocol: ..., herdr-auth.<utf8-hex>` for the standard
 *   WHATWG WebSocket client, which cannot set an Authorization header.
 *
 * Fail closed when configured secret is missing/empty.
 */
export class SharedSecretLinkAuthenticator implements LinkAuthenticator {
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
    const extracted = extractLinkCredential(request);
    if (!extracted.ok) return extracted;
    const credentialMatched = constantTimeEqual(enc.encode(extracted.credential), enc.encode(this.secret));
    if (!credentialMatched) {
      return { ok: false, code: "link_auth_failed", reason: "credential rejected" };
    }
    return {
      ok: true,
      claims: { workstationId, principal: "link", secretVersion: this.secretVersion, authedAtMs: atMs },
    };
  }
}