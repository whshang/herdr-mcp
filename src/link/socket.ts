/**
 * Socket boundary for `herdr-link`.
 *
 * The client only depends on the tiny `LinkWebSocket` surface, so tests can
 * drive a fake socket and a future VPS/Docker edge can reuse the same code.
 * `createStandardWebSocket` is the production default: it uses the platform's
 * built-in WHATWG `WebSocket` (present in Node.js >= 22 via `globalThis`),
 * which is exactly the surface Cloudflare Worker expects from its client-side
 * bindings too.
 */

import type { LinkWebSocket } from "./types.js";

/** Constructor shape of the platform WebSocket used by the default factory. */
type WebSocketConstructor = new (url: string, protocols?: string | string[]) => LinkWebSocket;

/**
 * Production socket factory using the runtime's global `WebSocket`.
 * Throws with a clear message if the platform does not provide one.
 */
export function createStandardWebSocket(url: string, protocols?: readonly string[]): LinkWebSocket {
  const Ctor = (globalThis as { WebSocket?: unknown }).WebSocket as WebSocketConstructor | undefined;
  if (typeof Ctor !== "function") {
    throw new Error("herdr-link: no global WebSocket available (Node.js >= 22 or explicit socketFactory required)");
  }
  return new Ctor(url, protocols ? [...protocols] : undefined) as LinkWebSocket;
}

/** Encode an arbitrary UTF-8 link secret as an RFC token-safe WS subprotocol. */
export function buildLinkAuthProtocol(linkToken: string): string {
  const bytes = new TextEncoder().encode(linkToken);
  let hex = "";
  for (const byte of bytes) hex += byte.toString(16).padStart(2, "0");
  return `herdr-auth.${hex}`;
}

export function buildLinkProtocols(protocolId: string, linkToken: string): string[] {
  return linkToken ? [protocolId, buildLinkAuthProtocol(linkToken)] : [protocolId];
}

/**
 * Build the actual WSS URL for the edge. Only workstation identity is placed
 * in the URL; credentials travel in Sec-WebSocket-Protocol via
 * buildLinkProtocols(), keeping link secrets out of URLs and access logs.
 */
export function buildEdgeUrl(
  baseUrl: string,
  identity: { workstationId: string; linkToken: string },
): string {
  const u = new URL(baseUrl);
  const basePath = u.pathname.replace(/\/+$/, "");
  const encodedWorkstation = encodeURIComponent(identity.workstationId);
  if (!basePath.endsWith(`/${encodedWorkstation}`)) {
    u.pathname = `${basePath || "/ws"}/${encodedWorkstation}`;
  }
  u.searchParams.delete("workstation_id");
  u.searchParams.delete("link_token");
  return u.toString();
}