/**
 * index.ts — Cloudflare Worker entry for Herdr Edge.
 *
 * Routes:
 *
 *   GET  /health                          edge health (no DO involved)
 *   GET  /info                            route/stage table for debugging
 *   GET  /status/:workstationId           DO presence snapshot (dev-open)
 *   GET  /ws/:workstationId               workstation link WSS upgrade (auth)
 *   POST /artifacts  GET|DELETE /artifacts/:id   private R2 generic artifact relay
 *   GET  /mcp  POST /mcp                  public MCP transport
 *   /.well-known/*                        OAuth / MCP discovery
 *
 * The workstation-link bearer check happens HERE (before the DO); the DO then
 * binds hello.workstationId to the route key and enforces protocol version.
 */

import { handleArtifactRequest, sweepExpiredArtifacts } from "./artifact-relay.js";
import { authenticateStaticMcpBearer, SharedSecretLinkAuthenticator, hasLinkApplicationProtocol } from "./auth.js";
import type { Env } from "./env.js";
import { errorResult } from "./errors.js";
import { edgeIdentity, MCP_SERVER_VERSION } from "./version.js";
import { handleMcp } from "./mcp-handler.js";
import {
  createSessionlessMcpProbeResponse,
  serializeMcpResponse,
} from "./mcp-chatgpt-transport.js";
import { readBodyBounded } from "./payload.js";
import { makeLimits } from "./limits.js";
import { createLogger } from "./logger.js";
import { WorkstationDO } from "./workstation-do.js";
import { OAuthStoreDO } from "./oauth-store-do.js";
import { DeviceRegistryDO } from "./device-registry-do.js";
import {
  ensureLegacyDeviceRegistration,
  listPublicDevices,
  resolveDeviceRouteWithContext,
} from "./device-directory.js";
import { authenticateMcpRequest } from "./oauth-mcp-auth.js";
import { createOAuthIdentity } from "./oauth-edge.js";
import { createOAuthPublicStore, handleOAuthPublic } from "./oauth-public.js";

export { DeviceRegistryDO, OAuthStoreDO, WorkstationDO };

const logger = createLogger("edge-worker");

function jsonResponse(payload: unknown, status = 200): Response {
  return new Response(JSON.stringify(payload), {
    status,
    headers: { "content-type": "application/json" },
  });
}

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    const url = new URL(request.url);
    const identity = edgeIdentity({
      edgeEnv: env.EDGE_ENV,
      edgeProject: env.EDGE_PROJECT,
      edgeVersion: env.EDGE_VERSION,
    });

    // ---- Public OAuth/discovery surface. This runs before MCP auth so
    // discovery, DCR, authorize and token exchange remain reachable exactly
    // like the current localhost runtime.
    const oauthResponse = await handleEdgeOAuthPublic(request, env);
    if (oauthResponse) return oauthResponse;

    // ---- /health: stable edge role, no DO dependency.
    if (request.method === "GET" && url.pathname === "/health") {
      return jsonResponse({
        ok: true,
        service: identity.edgeProject,
        stage: identity.edgeEnv,
        edgeVersion: identity.edgeVersion,
        edgeEnv: identity.edgeEnv,
        contractEpoch: identity.contractEpoch,
        contractHash: identity.contractHash,
        runtimeContractEpoch: identity.runtimeContractEpoch,
        runtimeContractHash: identity.runtimeContractHash,
        timestampMs: Date.now(),
      });
    }

    // ---- /info: route + stage table.
    if (request.method === "GET" && url.pathname === "/info") {
      return jsonResponse({
        ok: true,
        service: identity.edgeProject,
        stage: identity.edgeEnv,
        edgeVersion: identity.edgeVersion,
        publicContract: { epoch: identity.contractEpoch, hash: identity.contractHash },
        runtimeContract: { epoch: identity.runtimeContractEpoch, hash: identity.runtimeContractHash },
        routes: [
          { path: "/health", stage: "stable" },
          { path: "/info", stage: "dev" },
          { path: "/ws/:workstationId", stage: "dev (WS upgrade, link auth)" },
          { path: "/status/:workstationId", stage: "dev (DO presence)" },
          { path: "/mcp", stage: `public MCP epoch-${identity.contractEpoch} + sessionless ChatGPT SSE` },
          { path: "/artifacts", stage: "private R2 generic artifact relay (auth + capability)" },
          { path: "/.well-known/mcp.json", stage: "public MCP discovery" },
          { path: "/.well-known/oauth-*", stage: "public OAuth discovery" },
        ],
      });
    }

    // ---- One-time OAuth state import/admin. The route is effectively absent
    // when OAUTH_IMPORT_SECRET is unset; the secret is deleted after migration.
    if (url.pathname.startsWith("/__admin/oauth/")) {
      return handleOAuthAdmin(request, env);
    }

    // ---- Authenticated, non-sensitive OAuth state diagnostics.
    if (request.method === "GET" && url.pathname === "/status/oauth") {
      const auth = await authenticateEdgeMcpRequest(request, env);
      if (!auth.ok) return jsonResponse({ ok: false, code: "mcp_auth_failed" }, 401);
      const stub = env.OAUTH_STORE_DO.get(env.OAUTH_STORE_DO.idFromName("oauth-v1"));
      const response = await stub.fetch(new Request("https://oauth.internal/internal/oauth/stats"));
      const stats = await response.json() as Record<string, unknown>;
      return jsonResponse({
        ok: response.ok,
        issuer: env.OAUTH_ISSUER ?? null,
        clients: stats.clients ?? 0,
        access: stats.access ?? 0,
        refresh: stats.refresh ?? 0,
        codes: stats.codes ?? 0,
      }, response.ok ? 200 : 503);
    }

    // ---- /status/:workstationId — dev presence via DO.
    const statusMatch = /^\/status\/([^/]+)$/.exec(url.pathname);
    if (request.method === "GET" && statusMatch) {
      const devAuth = await authenticateEdgeMcpRequest(request, env);
      if (!devAuth.ok) return jsonResponse(errorResult("link_auth_failed", { message: "dev MCP authorization required" }), 401);
      const workstationId = decodeURIComponent(statusMatch[1]);
      const stub = env.WORKSTATION_DO.get(env.WORKSTATION_DO.idFromName(workstationId));
      try {
        const internal = new Request("https://do.internal/internal/status", { method: "GET" });
        const internalResp = await stub.fetch(internal);
        return new Response(internalResp.body, {
          status: internalResp.status,
          headers: { "content-type": "application/json" },
        });
      } catch (e) {
        logger.warn("status.do_error", { workstationId, error: String(e) });
        return jsonResponse({ ok: false, code: "internal_error", retryable: false, error: String(e) }, 503);
      }
    }

    // ---- /ws/:workstationId — workstation link WSS upgrade (auth first).
    const wsMatch = /^\/ws\/([^/]+)$/.exec(url.pathname);
    if (request.method === "GET" && wsMatch) {
      const workstationId = decodeURIComponent(wsMatch[1]);
      const upgrade = request.headers.get("Upgrade")?.toLowerCase();
      if (upgrade !== "websocket") {
        return jsonResponse(errorResult("bad_request", { message: "expected websocket upgrade" }), 400);
      }
      if (!hasLinkApplicationProtocol(request)) {
        return jsonResponse(errorResult("bad_request", { message: "missing herdr-link.v1 websocket subprotocol" }), 400);
      }
      const authenticator = new SharedSecretLinkAuthenticator({ secret: env.LINK_SHARED_SECRET });
      const decision = authenticator.authenticate(request, workstationId, Date.now());
      if (!decision.ok) {
        logger.warn("ws.upgrade.denied", { workstationId, code: decision.code });
        return jsonResponse(errorResult("link_auth_failed", { message: decision.reason, workstationId }), 401);
      }
      if (env.DEFAULT_WORKSTATION_ID === workstationId) {
        try {
          const registry = env.DEVICE_REGISTRY_DO.get(env.DEVICE_REGISTRY_DO.idFromName("devices-v1"));
          const registered = await ensureLegacyDeviceRegistration(registry, workstationId);
          if (registered.created) {
            logger.info("device.legacy_registered", { workstationId, deviceId: registered.device_id });
          }
        } catch (error) {
          // Preserve existing Link availability during rollout; a later reconnect retries registration.
          logger.warn("device.legacy_registration_failed", { workstationId, error: String(error) });
        }
      }
      // Route to the workstation DO — hibernation-safe accept happens there.
      const stub = env.WORKSTATION_DO.get(env.WORKSTATION_DO.idFromName(workstationId));
      return stub.fetch(request);
    }

    // ---- Private generic R2 artifact relay. Not an MCP tool; not public.
    const artifactResponse = await handleArtifactRequest(request, env, {
      verifyEdgeToken: (token) => verifyEdgeAccessToken(env, token),
    });
    if (artifactResponse) return artifactResponse;

    // ---- /mcp + unknown well-known fallback.
    if (url.pathname === "/mcp" || url.pathname.startsWith("/mcp/") || url.pathname.startsWith("/.well-known/")) {
      return handleMcpRouter(request, env);
    }

    return jsonResponse({ ok: false, code: "not_found", retryable: false, path: url.pathname }, 404);
  },

  async scheduled(_event: ScheduledEvent, env: Env): Promise<void> {
    if (!env.ARTIFACT_BUCKET) return;
    const result = await sweepExpiredArtifacts(env.ARTIFACT_BUCKET, Date.now());
    logger.info("artifact.sweep", { scanned: result.scanned, deleted: result.deleted });
  },
};

/** MCP-facing router. OAuth/discovery is handled before this function. */
async function handleMcpRouter(request: Request, env: Env): Promise<Response> {
  const url = new URL(request.url);
  const limits = makeLimits(env);
  const isMcpPath = url.pathname === "/mcp" || url.pathname === "/mcp/";

  if (request.method === "OPTIONS" && isMcpPath) {
    return new Response(null, { status: 204, headers: mcpCorsHeaders() });
  }

  const devAuth = await authenticateEdgeMcpRequest(request, env);
  if (!devAuth.ok) {
    return mcpUnauthorized(env);
  }

  if (request.method === "GET" && isMcpPath) {
    return withMcpCors(createSessionlessMcpProbeResponse({ signal: request.signal }));
  }
  if (request.method === "POST" && isMcpPath) {
    const parsed = await readBodyBounded(request, limits.maxFrameBytes);
    if (!parsed.ok) {
      return withMcpCors(jsonResponse(
        { ok: false, code: parsed.code, retryable: false, reason: parsed.reason },
        parsed.code === "payload_too_large" ? 413 : 400,
      ));
    }
    const workstationId = resolveWorkstation(request, env);
    const dev = await handleMcp(parsed.value, workstationId, {
      limits,
      client: {
        userAgent: request.headers.get("user-agent"),
        oauthClientId: devAuth.clientId ?? null,
      },
      forward: async (stub: unknown, body: string) => {
        const internal = new Request("https://do.internal/internal/forward", {
          method: "POST",
          headers: { "content-type": "application/json" },
          body,
        });
        return (stub as { fetch(r: Request): Promise<Response> }).fetch(internal);
      },
      getStub: (id: string) => env.WORKSTATION_DO.get(env.WORKSTATION_DO.idFromName(id)),
      listDevices: async () => {
        const registry = env.DEVICE_REGISTRY_DO.get(env.DEVICE_REGISTRY_DO.idFromName("devices-v1"));
        return listPublicDevices(
          registry,
          (id) => env.WORKSTATION_DO.get(env.WORKSTATION_DO.idFromName(id)),
        );
      },
      resolveDevice: async (selector, args) => {
        const registry = env.DEVICE_REGISTRY_DO.get(env.DEVICE_REGISTRY_DO.idFromName("devices-v1"));
        return resolveDeviceRouteWithContext(registry, { selector, args: args as Record<string, unknown> | undefined, legacyWorkstationId: workstationId });
      },
      logger,
    });
    const method =
      parsed.value !== null &&
      typeof parsed.value === "object" &&
      !Array.isArray(parsed.value) &&
      typeof (parsed.value as Record<string, unknown>).method === "string"
        ? ((parsed.value as Record<string, unknown>).method as string)
        : "";
    return withMcpCors(serializeMcpResponse(dev, {
      userAgent: request.headers.get("user-agent"),
      oauthClientId: devAuth.clientId ?? null,
      method,
    }));
  }
  if (url.pathname.startsWith("/.well-known/")) {
    return jsonResponse({ ok: false, code: "not_found", retryable: false, path: url.pathname }, 404);
  }
  return jsonResponse({ ok: false, code: "method_not_allowed", retryable: false, path: url.pathname }, 405);
}

async function handleEdgeOAuthPublic(request: Request, env: Env): Promise<Response | null> {
  if (!env.OAUTH_ISSUER) return null;
  const stub = env.OAUTH_STORE_DO.get(env.OAUTH_STORE_DO.idFromName("oauth-v1"));
  return handleOAuthPublic(request, {
    identity: createOAuthIdentity(env.OAUTH_ISSUER),
    store: createOAuthPublicStore(stub),
    fetchFn: globalThis.fetch,
    serverName: "herdr-mcp",
    serverVersion: MCP_SERVER_VERSION,
  });
}

function mcpCorsHeaders(): Headers {
  return new Headers({
    "access-control-allow-origin": "*",
    "access-control-allow-methods": "GET,HEAD,POST,OPTIONS",
    "access-control-allow-headers": "Authorization, Content-Type, Accept, Mcp-Session-Id, Mcp-Protocol-Version",
    "access-control-expose-headers": "WWW-Authenticate, Mcp-Session-Id",
    "access-control-max-age": "86400",
  });
}

function withMcpCors(response: Response): Response {
  const headers = new Headers(response.headers);
  for (const [name, value] of mcpCorsHeaders()) headers.set(name, value);
  return new Response(response.body, {
    status: response.status,
    statusText: response.statusText,
    headers,
  });
}

function mcpUnauthorized(env: Env): Response {
  const issuer = env.OAUTH_ISSUER?.replace(/\/+$/, "");
  const headers = mcpCorsHeaders();
  headers.set("content-type", "application/json");
  if (issuer) {
    headers.set(
      "www-authenticate",
      `Bearer resource_metadata="${issuer}/.well-known/oauth-protected-resource/mcp", scope="mcp"`,
    );
  }
  return new Response(
    JSON.stringify({ jsonrpc: "2.0", error: { code: -32000, message: "unauthorized" } }),
    { status: 401, headers },
  );
}

function resolveWorkstation(request: Request, env: Env): string {
  const fromHeader = request.headers.get("x-herdr-workstation");
  if (fromHeader && /^[A-Za-z0-9_.-]{1,64}$/.test(fromHeader)) return fromHeader;
  const fromQuery = new URL(request.url).searchParams.get("workstation");
  if (fromQuery && /^[A-Za-z0-9_.-]{1,64}$/.test(fromQuery)) return fromQuery;
  if (env.DEFAULT_WORKSTATION_ID && /^[A-Za-z0-9_.-]{1,64}$/.test(env.DEFAULT_WORKSTATION_ID)) {
    return env.DEFAULT_WORKSTATION_ID;
  }
  return "dev-ws1";
}

async function handleOAuthAdmin(request: Request, env: Env): Promise<Response> {
  if (!env.OAUTH_IMPORT_SECRET) return jsonResponse({ ok: false, code: "not_found" }, 404);
  if (!authenticateStaticMcpBearer(request, env.OAUTH_IMPORT_SECRET).ok) {
    return jsonResponse({ ok: false, code: "admin_auth_failed" }, 401);
  }
  const stub = env.OAUTH_STORE_DO.get(env.OAUTH_STORE_DO.idFromName("oauth-v1"));
  const url = new URL(request.url);

  if (request.method === "GET" && url.pathname === "/__admin/oauth/stats") {
    const response = await stub.fetch(new Request("https://oauth.internal/internal/oauth/stats"));
    return new Response(response.body, {
      status: response.status,
      headers: { "content-type": "application/json", "cache-control": "no-store" },
    });
  }

  if (request.method === "POST" && url.pathname === "/__admin/oauth/import") {
    const parsed = await readBodyBounded(request, 256 * 1024);
    if (!parsed.ok) {
      return jsonResponse(
        { ok: false, code: parsed.code },
        parsed.code === "payload_too_large" ? 413 : 400,
      );
    }
    const response = await stub.fetch(new Request("https://oauth.internal/internal/oauth/import", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(parsed.value),
    }));
    return new Response(response.body, {
      status: response.status,
      headers: { "content-type": "application/json", "cache-control": "no-store" },
    });
  }

  return jsonResponse({ ok: false, code: "not_found" }, 404);
}

async function verifyEdgeAccessToken(env: Env, token: string): Promise<{ ok: boolean; clientId?: string }> {
  const stub = env.OAUTH_STORE_DO.get(env.OAUTH_STORE_DO.idFromName("oauth-v1"));
  const response = await stub.fetch(new Request("https://oauth.internal/internal/oauth/access/verify", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ token, now_sec: Math.floor(Date.now() / 1000) }),
  }));
  if (!response.ok) return { ok: false };
  const payload = await response.json() as Record<string, unknown>;
  const clientId = typeof payload.client_id === "string" ? payload.client_id : undefined;
  return clientId ? { ok: true, clientId } : { ok: payload.ok === true };
}

async function authenticateEdgeMcpRequest(request: Request, env: Env) {
  return authenticateMcpRequest(request, env, {
    verifyEdgeToken: (token) => verifyEdgeAccessToken(env, token),
  });
}