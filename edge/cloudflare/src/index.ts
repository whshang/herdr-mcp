/**
 * index.ts — Cloudflare Worker entry for Herdr Edge.
 *
 * Routes:
 *
 *   GET  /health                          edge health (no DO involved)
 *   GET  /info                            route/stage table for debugging
 *   GET  /status/:workstationId           DO presence snapshot (dev-open)
 *   GET  /devices                         owner-authenticated device inventory
 *   POST /devices/pairings               owner-authenticated pairing session creation
 *   POST /devices/pairings/consume       one-time pairing consumption by a new device
 *   GET  /ws/:workstationId               workstation link WSS upgrade (auth)
 *   POST /artifacts  GET|DELETE /artifacts/:id   private R2 generic artifact relay
 *   GET  /mcp  POST /mcp                  public MCP transport
 *   /.well-known/*                        OAuth / MCP discovery
 *
 * The workstation-link bearer check happens HERE (before the DO); the DO then
 * binds hello.workstationId to the route key and enforces protocol version.
 */

import { handleArtifactRequest, sweepExpiredArtifacts } from "./artifact-relay.js";
import {
  authenticateStaticMcpBearer,
  extractLinkCredential,
  SharedSecretLinkAuthenticator,
  hasLinkApplicationProtocol,
} from "./auth.js";
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
  authenticateDeviceCredential,
  consumePairingSession,
  createPairingSession,
  ensureLegacyDeviceRegistration,
  listPublicDevices,
  renameRegisteredDevice,
  resolveDeviceRouteWithContext,
  revokeRegisteredDevice,
  resolveDeviceRoute,
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

function noStoreJsonResponse(payload: unknown, status = 200): Response {
  return new Response(JSON.stringify(payload), {
    status,
    headers: {
      "content-type": "application/json",
      "cache-control": "no-store",
      pragma: "no-cache",
    },
  });
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function deviceNameFromLinkRequest(request: Request): string | undefined {
  const encoded = request.headers.get("x-herdr-device-name-b64");
  if (!encoded || encoded.length > 1024 || !/^[A-Za-z0-9_-]+$/.test(encoded)) return undefined;
  try {
    const b64 = encoded.replace(/-/g, "+").replace(/_/g, "/");
    const pad = b64.length % 4 === 0 ? "" : "=".repeat(4 - (b64.length % 4));
    const binary = atob(b64 + pad);
    const bytes = new Uint8Array(binary.length);
    for (let index = 0; index < binary.length; index++) bytes[index] = binary.charCodeAt(index);
    const name = new TextDecoder("utf-8", { fatal: true, ignoreBOM: false }).decode(bytes).trim();
    return name.length > 0 && name.length <= 128 ? name : undefined;
  } catch {
    return undefined;
  }
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
          { path: "/devices/revoke-self", stage: "device self-revoke (exact credential binding)" },
          { path: "/devices/revoke", stage: "owner/operator revoke of any enrolled device" },
          { path: "/devices", stage: "owner-authenticated device inventory" },
          { path: "/devices/pairings", stage: "owner-authenticated device pairing creation" },
          { path: "/devices/pairings/consume", stage: "one-time device pairing consumption" },
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

    // ---- Device pairing control plane. Pairing creation requires
    // owner/operator auth; consumption requires only the raw pairing_id plus
    // the six-digit code, so a second workstation never needs Cloudflare
    // deploy credentials. Raw pairing material is returned once and never
    // stored or logged; the DO keeps only digest-keyed, HMAC-bound verifiers.
    // The six-digit code NEVER travels in a URL/URI/query — consumption is
    // JSON-body-only; only the pairing_id may appear in a descriptor/fragment.
    if (request.method === "GET" && url.pathname === "/devices") {
      const ownerAuth = await authenticateOwner(request, env);
      if (!ownerAuth) return noStoreJsonResponse({ ok: false, code: "device_inventory_admin_required" }, 401);
      const registry = env.DEVICE_REGISTRY_DO.get(env.DEVICE_REGISTRY_DO.idFromName("devices-v1"));
      try {
        const devices = await listPublicDevices(
          registry,
          (workstationId) => env.WORKSTATION_DO.get(env.WORKSTATION_DO.idFromName(workstationId)),
        );
        return noStoreJsonResponse({ ok: true, devices, observed_at_ms: Date.now() });
      } catch {
        return noStoreJsonResponse({ ok: false, code: "device_registry_unavailable" }, 503);
      }
    }

    if (request.method === "POST" && url.pathname === "/devices/pairings") {
      const ownerAuth = await authenticateOwner(request, env);
      if (!ownerAuth) return noStoreJsonResponse({ ok: false, code: "pairing_admin_required" }, 401);
      const parsed = await readBodyBounded(request, 8 * 1024);
      if (!parsed.ok || !isRecord(parsed.value)) {
        const code = parsed.ok ? "bad_request" : parsed.code;
        return noStoreJsonResponse({ ok: false, code }, !parsed.ok && parsed.code === "payload_too_large" ? 413 : 400);
      }
      const input: { ttl_seconds?: number; name?: string; worker_context: string } = {
        worker_context: pairingWorkerContext(env),
      };
      if (parsed.value.ttl_seconds !== undefined) input.ttl_seconds = parsed.value.ttl_seconds as number;
      if (parsed.value.name !== undefined) input.name = parsed.value.name as string;
      const registry = env.DEVICE_REGISTRY_DO.get(env.DEVICE_REGISTRY_DO.idFromName("devices-v1"));
      const result = await createPairingSession(registry, input);
      return result.ok
        ? noStoreJsonResponse({
            ok: true,
            pairing_id: result.pairing.pairing_id,
            code: result.pairing.code,
            expires_at_ms: result.pairing.expires_at_ms,
            worker_origin: url.origin,
          })
        : noStoreJsonResponse({ ok: false, code: result.code }, result.status);
    }

    if (request.method === "POST" && url.pathname === "/devices/pairings/consume") {
      const parsed = await readBodyBounded(request, 8 * 1024);
      if (!parsed.ok || !isRecord(parsed.value) || typeof parsed.value.pairing_id !== "string" || typeof parsed.value.code !== "string") {
        const code = parsed.ok ? "bad_request" : parsed.code;
        return noStoreJsonResponse({ ok: false, code }, !parsed.ok && parsed.code === "payload_too_large" ? 413 : 400);
      }
      const input: { pairing_id: string; code: string; name?: string; worker_context: string } = {
        pairing_id: parsed.value.pairing_id,
        code: parsed.value.code,
        worker_context: pairingWorkerContext(env),
      };
      if (parsed.value.name !== undefined) input.name = parsed.value.name as string;
      const registry = env.DEVICE_REGISTRY_DO.get(env.DEVICE_REGISTRY_DO.idFromName("devices-v1"));
      const result = await consumePairingSession(registry, input);
      return result.ok
        ? noStoreJsonResponse({
            ok: true,
            device_id: result.credential.device_id,
            workstation_id: result.credential.workstation_id,
            credential_id: result.credential.credential_id,
            device_secret: result.credential.device_secret,
          })
        : noStoreJsonResponse({ ok: false, code: result.code }, result.status);
    }

    if (request.method === "POST" && url.pathname === "/devices/revoke-self") {
      const parsed = await readBodyBounded(request, 8 * 1024);
      if (!parsed.ok || !isRecord(parsed.value) || typeof parsed.value.workstation_id !== "string") {
        const code = parsed.ok ? "bad_request" : parsed.code;
        return noStoreJsonResponse({ ok: false, code }, !parsed.ok && parsed.code === "payload_too_large" ? 413 : 400);
      }
      const workstationId = parsed.value.workstation_id.trim();
      if (!workstationId || !/^[A-Za-z0-9_.-]{1,64}$/.test(workstationId)) {
        return noStoreJsonResponse({ ok: false, code: "bad_request" }, 400);
      }
      const extracted = extractLinkCredential(request);
      if (!extracted.ok) return noStoreJsonResponse({ ok: false, code: "link_auth_failed" }, 401);
      const registry = env.DEVICE_REGISTRY_DO.get(env.DEVICE_REGISTRY_DO.idFromName("devices-v1"));
      const authenticated = await authenticateDeviceCredential(registry, workstationId, extracted.credential);
      if (!authenticated.ok) return noStoreJsonResponse({ ok: false, code: authenticated.code }, 401);
      const revoked = await revokeRegisteredDevice(registry, authenticated.device_id);
      if (revoked.ok) return noStoreJsonResponse({ ok: true, device_id: revoked.device_id });
      return noStoreJsonResponse(
        { ok: false, code: revoked.code, retryable: revoked.retryable },
        revoked.code === "revoke_teardown_failed" ? 503 : 404,
      );
    }

    if (request.method === "POST" && url.pathname === "/devices/rename-self") {
      const parsed = await readBodyBounded(request, 8 * 1024);
      if (!parsed.ok || !isRecord(parsed.value) || typeof parsed.value.workstation_id !== "string" || typeof parsed.value.name !== "string") {
        const code = parsed.ok ? "bad_request" : parsed.code;
        return noStoreJsonResponse({ ok: false, code }, !parsed.ok && parsed.code === "payload_too_large" ? 413 : 400);
      }
      const workstationId = parsed.value.workstation_id.trim();
      const name = parsed.value.name.trim();
      if (!workstationId || !/^[A-Za-z0-9_.-]{1,64}$/.test(workstationId) || !name || name.length > 128) {
        return noStoreJsonResponse({ ok: false, code: "bad_request" }, 400);
      }
      const extracted = extractLinkCredential(request);
      if (!extracted.ok) return noStoreJsonResponse({ ok: false, code: "link_auth_failed" }, 401);
      const registry = env.DEVICE_REGISTRY_DO.get(env.DEVICE_REGISTRY_DO.idFromName("devices-v1"));
      const deviceAuth = await authenticateDeviceCredential(registry, workstationId, extracted.credential);
      let authorized = deviceAuth.ok;
      if (!authorized) {
        const ownerAuth = await authenticateOwner(request, env);
        const ownerWorkstation = request.headers.get("x-herdr-workstation")?.trim() ?? "";
        authorized = ownerAuth
          && workstationId === env.DEFAULT_WORKSTATION_ID
          && ownerWorkstation === workstationId;
      }
      if (!authorized) return noStoreJsonResponse({ ok: false, code: "rename_auth_failed" }, 401);

      const renamed = await renameRegisteredDevice(registry, workstationId, name);
      if (renamed.ok) {
        return noStoreJsonResponse({
          ok: true,
          device_id: renamed.device_id,
          name: renamed.name,
          updated_at_ms: renamed.updated_at_ms,
          wrote_registry: renamed.wrote_registry,
        });
      }
      const status = renamed.code === "device_not_found" ? 404
        : renamed.code === "device_revoked" || renamed.code === "registry_corrupt" ? 409
          : renamed.code === "invalid_device_name" ? 400
            : 503;
      return noStoreJsonResponse({ ok: false, code: renamed.code }, status);
    }

    // ---- Owner/operator revoke of any enrolled device. The caller supplies only
    // the canonical target device_id — never a workstation_id or target secret.
    // Authorization is the same trusted owner contract used for pairing
    // creation: trusted MCP/OAuth/operator auth, or the exact default-workstation
    // link credential. A joined member device credential is never sufficient
    // unless its authenticated workstation is exactly DEFAULT_WORKSTATION_ID.
    if (request.method === "POST" && url.pathname === "/devices/revoke") {
      const ownerAuth = await authenticateOwner(request, env);
      if (!ownerAuth) return noStoreJsonResponse({ ok: false, code: "revoke_admin_required" }, 401);
      const parsed = await readBodyBounded(request, 8 * 1024);
      if (!parsed.ok || !isRecord(parsed.value) || typeof parsed.value.device_id !== "string") {
        const code = parsed.ok ? "bad_request" : parsed.code;
        return noStoreJsonResponse({ ok: false, code }, !parsed.ok && parsed.code === "payload_too_large" ? 413 : 400);
      }
      const deviceId = parsed.value.device_id.trim();
      if (!deviceId) return noStoreJsonResponse({ ok: false, code: "bad_request" }, 400);
      const registry = env.DEVICE_REGISTRY_DO.get(env.DEVICE_REGISTRY_DO.idFromName("devices-v1"));
      const revoked = await revokeRegisteredDevice(registry, deviceId);
      if (revoked.ok) {
        return noStoreJsonResponse({ ok: true, device_id: revoked.device_id, revoked_at_ms: revoked.revoked_at_ms });
      }
      if (revoked.code === "device_not_found") return noStoreJsonResponse({ ok: false, code: "device_not_found" }, 404);
      if (revoked.code === "invalid_device_id") return noStoreJsonResponse({ ok: false, code: "invalid_device_id" }, 400);
      return noStoreJsonResponse({ ok: false, code: revoked.code, retryable: revoked.retryable }, 503);
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
      const registry = env.DEVICE_REGISTRY_DO.get(env.DEVICE_REGISTRY_DO.idFromName("devices-v1"));
      const extracted = extractLinkCredential(request);
      const deviceAuth = extracted.ok
        ? await authenticateDeviceCredential(registry, workstationId, extracted.credential)
        : { ok: false as const, code: "link_auth_failed" as const };
      let authenticatedBy: "device" | "legacy" | null = deviceAuth.ok ? "device" : null;

      if (!authenticatedBy && !deviceAuth.ok && env.DEFAULT_WORKSTATION_ID === workstationId) {
        const legacyEligible = deviceAuth.code === "device_not_found" || deviceAuth.code === "device_credential_missing";
        if (legacyEligible) {
          const authenticator = new SharedSecretLinkAuthenticator({ secret: env.LINK_SHARED_SECRET });
          const legacyDecision = authenticator.authenticate(request, workstationId, Date.now());
          if (legacyDecision.ok) authenticatedBy = "legacy";
        }
      }

      if (!authenticatedBy) {
        const code = !deviceAuth.ok ? deviceAuth.code : "link_auth_failed";
        logger.warn("ws.upgrade.denied", { workstationId, code });
        return jsonResponse(errorResult("link_auth_failed", { message: "credential rejected", workstationId }), 401);
      }

      if (authenticatedBy === "legacy") {
        try {
          const registered = await ensureLegacyDeviceRegistration(
            registry,
            workstationId,
            deviceNameFromLinkRequest(request),
          );
          if (registered.created) {
            logger.info("device.legacy_registered", { workstationId, deviceId: registered.device_id });
          }
        } catch (error) {
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
      createPairing: async (input) => {
        const registry = env.DEVICE_REGISTRY_DO.get(env.DEVICE_REGISTRY_DO.idFromName("devices-v1"));
        const pairingInput = {
          worker_context: pairingWorkerContext(env),
          ttl_seconds: input?.ttl_seconds,
          name: input?.name,
        };
        const result = await createPairingSession(registry, pairingInput);
        if (!result.ok) {
          return { ok: false, code: result.code, status: result.status };
        }
        const pairingAddress = `${url.origin}/pair#${result.pairing.pairing_id}`;
        return {
          ok: true,
          pairing_id: result.pairing.pairing_id,
          code: result.pairing.code,
          expires_at_ms: result.pairing.expires_at_ms,
          worker_origin: url.origin,
          pairing_address: pairingAddress,
        };
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

/**
 * Stable per-deployment context binding for pairing verifiers. Create and
 * consume requests must carry the same derived context, so a pairing minted by
 * one Worker/deployment can never be consumed against another. Cross-Worker
 * mismatches fail closed inside the registry DO.
 */
function pairingWorkerContext(env: Env): string {
  return `${env.EDGE_PROJECT ?? "herdr-edge"}@${env.EDGE_ENV ?? "dev"}`;
}

/**
 * Shared owner/operator authorization for the device control plane (pairing
 * creation and owner revoke). Accepted owner contracts:
 *  - trusted MCP/OAuth/operator auth (authenticateEdgeMcpRequest); or
 *  - the exact DEFAULT_WORKSTATION_ID link credential (device or legacy).
 * A joined member device credential is never sufficient unless its
 * authenticated workstation is exactly DEFAULT_WORKSTATION_ID.
 */
async function authenticateOwner(request: Request, env: Env): Promise<boolean> {
  const owner = await authenticateEdgeMcpRequest(request, env);
  if (owner.ok) return true;

  const workstationId = request.headers.get("x-herdr-workstation")?.trim() ?? "";
  if (!env.DEFAULT_WORKSTATION_ID || workstationId !== env.DEFAULT_WORKSTATION_ID) return false;
  const extracted = extractLinkCredential(request);
  if (!extracted.ok) return false;

  const registry = env.DEVICE_REGISTRY_DO.get(env.DEVICE_REGISTRY_DO.idFromName("devices-v1"));
  const deviceAuth = await authenticateDeviceCredential(registry, workstationId, extracted.credential);
  if (deviceAuth.ok) return true;
  if (deviceAuth.code !== "device_not_found" && deviceAuth.code !== "device_credential_missing") {
    return false;
  }
  const legacy = new SharedSecretLinkAuthenticator({ secret: env.LINK_SHARED_SECRET });
  return legacy.authenticate(request, workstationId, Date.now()).ok;
}