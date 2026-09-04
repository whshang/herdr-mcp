/**
 * index.ts — Cloudflare Worker entry for Herdr Edge.
 *
 * Routes:
 *
 *   GET  /health                          edge health (no DO involved)
 *   GET  /info                            route/stage table for debugging
 *   GET  /status/:workstationId           DO presence snapshot (dev-open)
 *   GET  /devices                         fleet-admin device inventory
 *   POST /devices/pairings               fleet-admin pairing session creation
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
import { createOAuthIdentity, hashOAuthApprovalCode } from "./oauth-edge.js";
import { createOAuthPublicStore, handleOAuthPublic } from "./oauth-public.js";
import { randomBase64UrlToken } from "./oauth-token-crypto.js";
import { sha256Hex } from "./device-crypto.js";

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
          { path: "/devices/revoke", stage: "fleet-admin revoke of any enrolled device" },
          { path: "/devices", stage: "fleet-admin device inventory" },
          { path: "/devices/pairings", stage: "fleet-admin device pairing creation" },
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
        approvals: stats.approvals ?? 0,
        grants: stats.grants ?? 0,
      }, response.ok ? 200 : 503);
    }

    // ---- Connector approval bootstrap. Fleet administration belongs to the
    // Worker, not to a privileged workstation. Any currently enrolled device,
    // explicit v0.4.6+ Connector grant, or Worker operator credential may act
    // as an administration channel. Pre-v0.4.6 OAuth tokens were issued
    // without explicit consent and therefore never gain fleet-admin authority
    // merely by remaining valid for ordinary MCP compatibility.
    if (request.method === "POST" && url.pathname === "/connectors/inspect") {
      const fleetAdmin = await authenticateFleetAdmin(request, env);
      if (!fleetAdmin) return noStoreJsonResponse({ ok: false, code: "fleet_admin_required" }, 401);
      const parsed = await readBodyBounded(request, 8 * 1024);
      if (!parsed.ok || !isRecord(parsed.value) || typeof parsed.value.request_id !== "string") {
        const code = parsed.ok ? "bad_request" : parsed.code;
        return noStoreJsonResponse({ ok: false, code }, !parsed.ok && parsed.code === "payload_too_large" ? 413 : 400);
      }
      const requestId = parsed.value.request_id.trim();
      if (!requestId || requestId.length > 256) return noStoreJsonResponse({ ok: false, code: "invalid_connector_approval" }, 400);
      const result = await inspectConnectorRequest(env, requestId);
      return result.ok
        ? noStoreJsonResponse(result)
        : noStoreJsonResponse({ ok: false, code: result.code }, 404);
    }

    if (request.method === "POST" && url.pathname === "/connectors/approve") {
      const fleetAdmin = await authenticateFleetAdmin(request, env);
      if (!fleetAdmin) return noStoreJsonResponse({ ok: false, code: "fleet_admin_required" }, 401);
      const parsed = await readBodyBounded(request, 8 * 1024);
      if (!parsed.ok || !isRecord(parsed.value)) {
        const code = parsed.ok ? "bad_request" : parsed.code;
        return noStoreJsonResponse({ ok: false, code }, !parsed.ok && parsed.code === "payload_too_large" ? 413 : 400);
      }
      const requestId = typeof parsed.value.request_id === "string" ? parsed.value.request_id.trim() : "";
      const code = typeof parsed.value.code === "string" ? parsed.value.code.trim() : "";
      if (!requestId || requestId.length > 256 || !/^\d{6}$/.test(code)) {
        return noStoreJsonResponse({ ok: false, code: "invalid_connector_approval" }, 400);
      }
      const result = await approveConnectorRequest(env, requestId, code, fleetAdmin);
      return result.ok
        ? noStoreJsonResponse({ action: "connector_approve", ...result })
        : noStoreJsonResponse({ ok: false, code: result.code }, result.code === "invalid_code" ? 403 : result.code === "locked" ? 423 : 404);
    }

    if (request.method === "POST" && url.pathname === "/connectors/revoke") {
      const fleetAdmin = await authenticateFleetAdmin(request, env);
      if (!fleetAdmin) return noStoreJsonResponse({ ok: false, code: "fleet_admin_required" }, 401);
      const parsed = await readBodyBounded(request, 8 * 1024);
      if (!parsed.ok || !isRecord(parsed.value) || typeof parsed.value.client_id !== "string") {
        const code = parsed.ok ? "bad_request" : parsed.code;
        return noStoreJsonResponse({ ok: false, code }, !parsed.ok && parsed.code === "payload_too_large" ? 413 : 400);
      }
      const clientId = parsed.value.client_id.trim();
      if (!clientId || clientId.length > 4096) return noStoreJsonResponse({ ok: false, code: "invalid_client_id" }, 400);
      const result = await revokeConnectorGrant(env, clientId, fleetAdmin);
      return result.ok
        ? noStoreJsonResponse({ ok: true, action: "connector_revoke", client_id: clientId })
        : noStoreJsonResponse({ ok: false, code: result.code }, result.code === "connector_grant_not_found" ? 404 : 500);
    }

    // ---- Non-interactive automation principals (GitLab CI, other CI/CD).
    // These are Worker-owned service principals, not global bearer secrets.
    // Their long-lived client_secret is returned only by create/rotate and is
    // never persisted in plaintext by the Worker. Automation principals may
    // call ordinary MCP but can never satisfy authenticateFleetAdmin().
    if (request.method === "GET" && url.pathname === "/automations") {
      const fleetAdmin = await authenticateFleetAdmin(request, env);
      if (!fleetAdmin) return noStoreJsonResponse({ ok: false, code: "fleet_admin_required" }, 401);
      const result = await listAutomationClients(env);
      return result.ok
        ? noStoreJsonResponse(result)
        : noStoreJsonResponse(result, 503);
    }

    if (request.method === "POST" && url.pathname === "/automations") {
      const fleetAdmin = await authenticateFleetAdmin(request, env);
      if (!fleetAdmin) return noStoreJsonResponse({ ok: false, code: "fleet_admin_required" }, 401);
      const parsed = await readBodyBounded(request, 8 * 1024);
      if (!parsed.ok || !isRecord(parsed.value) || typeof parsed.value.name !== "string" || typeof parsed.value.device !== "string") {
        const code = parsed.ok ? "bad_request" : parsed.code;
        return noStoreJsonResponse({ ok: false, code }, !parsed.ok && parsed.code === "payload_too_large" ? 413 : 400);
      }
      const name = parsed.value.name.trim();
      if (!name || name.length > 256) return noStoreJsonResponse({ ok: false, code: "invalid_automation_name" }, 400);
      const deviceSelector = parsed.value.device.trim();
      if (!deviceSelector) return noStoreJsonResponse({ ok: false, code: "invalid_automation_device" }, 400);
      const registry = env.DEVICE_REGISTRY_DO.get(env.DEVICE_REGISTRY_DO.idFromName("devices-v1"));
      const resolved = await resolveDeviceRouteWithContext(registry, {
        selector: deviceSelector,
        legacyWorkstationId: env.DEFAULT_WORKSTATION_ID ?? "dev-ws1",
      });
      if (!resolved.ok || !resolved.device_id) {
        return noStoreJsonResponse({ ok: false, code: "automation_device_not_routable" }, 400);
      }
      const result = await createAutomationClient(env, name, fleetAdmin, resolved.device_id, resolved.device_name ?? null);
      return result.ok
        ? noStoreJsonResponse(result, 201)
        : noStoreJsonResponse({ ok: false, code: result.code }, result.code === "oauth_not_configured" ? 503 : 409);
    }

    if (request.method === "POST" && url.pathname === "/automations/rotate") {
      const fleetAdmin = await authenticateFleetAdmin(request, env);
      if (!fleetAdmin) return noStoreJsonResponse({ ok: false, code: "fleet_admin_required" }, 401);
      const parsed = await readBodyBounded(request, 8 * 1024);
      if (!parsed.ok || !isRecord(parsed.value) || typeof parsed.value.client_id !== "string") {
        const code = parsed.ok ? "bad_request" : parsed.code;
        return noStoreJsonResponse({ ok: false, code }, !parsed.ok && parsed.code === "payload_too_large" ? 413 : 400);
      }
      const clientId = parsed.value.client_id.trim();
      if (!/^svc_[A-Za-z0-9_-]{8,128}$/.test(clientId)) return noStoreJsonResponse({ ok: false, code: "invalid_client_id" }, 400);
      const result = await rotateAutomationClient(env, clientId, fleetAdmin);
      return result.ok
        ? noStoreJsonResponse(result)
        : noStoreJsonResponse({ ok: false, code: result.code }, result.code === "automation_client_not_found" ? 404 : 409);
    }

    if (request.method === "POST" && url.pathname === "/automations/revoke") {
      const fleetAdmin = await authenticateFleetAdmin(request, env);
      if (!fleetAdmin) return noStoreJsonResponse({ ok: false, code: "fleet_admin_required" }, 401);
      const parsed = await readBodyBounded(request, 8 * 1024);
      if (!parsed.ok || !isRecord(parsed.value) || typeof parsed.value.client_id !== "string") {
        const code = parsed.ok ? "bad_request" : parsed.code;
        return noStoreJsonResponse({ ok: false, code }, !parsed.ok && parsed.code === "payload_too_large" ? 413 : 400);
      }
      const clientId = parsed.value.client_id.trim();
      if (!/^svc_[A-Za-z0-9_-]{8,128}$/.test(clientId)) return noStoreJsonResponse({ ok: false, code: "invalid_client_id" }, 400);
      const result = await revokeAutomationClient(env, clientId, fleetAdmin);
      return result.ok
        ? noStoreJsonResponse({ ok: true, action: "automation_revoke", client_id: clientId })
        : noStoreJsonResponse({ ok: false, code: result.code }, result.code === "automation_client_not_found" ? 404 : 409);
    }

    // ---- Device pairing control plane. Pairing creation requires Worker-owned
    // fleet-admin auth; consumption requires only the raw pairing_id plus
    // the six-digit code, so a second workstation never needs Cloudflare
    // deploy credentials. Raw pairing material is returned once and never
    // stored or logged; the DO keeps only digest-keyed, HMAC-bound verifiers.
    // The six-digit code NEVER travels in a URL/URI/query — consumption is
    // JSON-body-only; only the pairing_id may appear in a descriptor/fragment.
    if (request.method === "GET" && url.pathname === "/devices") {
      const fleetAdmin = await authenticateFleetAdmin(request, env);
      if (!fleetAdmin) return noStoreJsonResponse({ ok: false, code: "device_inventory_admin_required" }, 401);
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
      const fleetAdmin = await authenticateFleetAdmin(request, env);
      if (!fleetAdmin) return noStoreJsonResponse({ ok: false, code: "pairing_admin_required" }, 401);
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
        const fleetAdmin = await authenticateFleetAdmin(request, env);
        const presentedWorkstation = request.headers.get("x-herdr-workstation")?.trim() ?? "";
        authorized = fleetAdmin !== null
          && workstationId === env.DEFAULT_WORKSTATION_ID
          && presentedWorkstation === workstationId;
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

    // ---- Fleet-admin revoke of any enrolled device. The caller supplies only
    // the canonical target device_id — never a workstation_id or target secret.
    // Authorization is the same Worker-owned fleet-admin contract used for
    // pairing creation; enrolled devices have no owner/member hierarchy.
    if (request.method === "POST" && url.pathname === "/devices/revoke") {
      const fleetAdmin = await authenticateFleetAdmin(request, env);
      if (!fleetAdmin) return noStoreJsonResponse({ ok: false, code: "revoke_admin_required" }, 401);
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
  const mcpFleetPrincipal =
    devAuth.source === "dev_bearer" || devAuth.source === "static_bearer"
      ? `operator:${devAuth.source}`
      : null;

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
        automationDeviceId: devAuth.principalType === "automation" ? (devAuth.deviceId ?? null) : null,
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
        if (!mcpFleetPrincipal) {
          return { ok: false, code: "fleet_admin_required", status: 403 };
        }
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
      revokeDevice: async (deviceId) => {
        if (!mcpFleetPrincipal) {
          return { ok: false, code: "fleet_admin_required", retryable: false };
        }
        const registry = env.DEVICE_REGISTRY_DO.get(env.DEVICE_REGISTRY_DO.idFromName("devices-v1"));
        const result = await revokeRegisteredDevice(registry, deviceId);
        if (!result.ok) {
          return {
            ok: false,
            code: result.code,
            retryable: result.retryable,
          };
        }
        return {
          ok: true,
          device_id: result.device_id,
          revoked_at_ms: result.revoked_at_ms,
        };
      },
      approveConnector: async (input) => {
        if (!mcpFleetPrincipal) {
          return { ok: false, code: "fleet_admin_required" };
        }
        return approveConnectorRequest(
          env,
          input.request_id,
          input.code,
          mcpFleetPrincipal,
        );
      },
      revokeConnector: async (clientId) => {
        if (!mcpFleetPrincipal) {
          return { ok: false, code: "fleet_admin_required" };
        }
        return revokeConnectorGrant(env, clientId, mcpFleetPrincipal);
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
  if (!env.LINK_SHARED_SECRET) {
    const path = new URL(request.url).pathname;
    if (path === "/oauth/authorize" || path === "/oauth/authorize/poll") {
      return noStoreJsonResponse({
        error: "server_error",
        error_description: "OAuth fleet approval is not configured",
      }, 503);
    }
  }
  const stub = env.OAUTH_STORE_DO.get(env.OAUTH_STORE_DO.idFromName("oauth-v1"));
  return handleOAuthPublic(request, {
    identity: createOAuthIdentity(env.OAUTH_ISSUER),
    store: createOAuthPublicStore(stub),
    approvalSecret: env.LINK_SHARED_SECRET ?? "",
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

async function verifyEdgeAccessToken(env: Env, token: string): Promise<{ ok: boolean; clientId?: string; principalType?: string; deviceId?: string }> {
  const stub = env.OAUTH_STORE_DO.get(env.OAUTH_STORE_DO.idFromName("oauth-v1"));
  const response = await stub.fetch(new Request("https://oauth.internal/internal/oauth/access/verify", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ token, now_sec: Math.floor(Date.now() / 1000) }),
  }));
  if (!response.ok) return { ok: false };
  const payload = await response.json() as Record<string, unknown>;
  const clientId = typeof payload.client_id === "string" ? payload.client_id : undefined;
  const principalType = typeof payload.principal_type === "string" ? payload.principal_type : undefined;
  const deviceId = typeof payload.device_id === "string" ? payload.device_id : undefined;
  if (!clientId) return { ok: false };
  return {
    ok: true,
    clientId,
    ...(principalType ? { principalType } : {}),
    ...(deviceId ? { deviceId } : {}),
  };
}

async function authenticateEdgeMcpRequest(request: Request, env: Env) {
  return authenticateMcpRequest(request, env, {
    verifyEdgeToken: (token) => verifyEdgeAccessToken(env, token),
    verifyLegacyClient: (clientId) => verifyLegacyOAuthClientGrantFence(env, clientId),
  });
}

async function verifyLegacyOAuthClientGrantFence(env: Env, clientId: string): Promise<boolean> {
  const response = await oauthInternal(env, "/internal/oauth/grant/get", { client_id: clientId });
  if (response.status === 404) {
    // Pre-v0.4.6 clients have no grant record and retain ordinary MCP access
    // until an explicit revoke creates a durable tombstone.
    return true;
  }
  if (!response.ok) return false;
  const payload = await response.json().catch(() => null) as { record?: { status?: string } } | null;
  return payload?.record?.status === "active";
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
 * Worker-owned fleet administration. There is no owner/member device
 * hierarchy: any active enrolled device is an equivalent administration
 * channel. Worker operator credentials are also accepted. OAuth Connectors,
 * including explicitly approved v0.4.6+ instances, remain ordinary MCP
 * principals and never gain fleet administration merely by being approved.
 */
async function authenticateFleetAdmin(request: Request, env: Env): Promise<string | null> {
  const mcp = await authenticateEdgeMcpRequest(request, env);
  if (mcp.ok) {
    if (mcp.source === "dev_bearer" || mcp.source === "static_bearer") {
      return `operator:${mcp.source}`;
    }
  }
  return authenticateFleetDevice(request, env);
}

async function authenticateFleetDevice(request: Request, env: Env): Promise<string | null> {
  const workstationId = request.headers.get("x-herdr-workstation")?.trim() ?? "";
  if (!workstationId || !/^[A-Za-z0-9_.-]{1,64}$/.test(workstationId)) return null;
  const extracted = extractLinkCredential(request);
  if (!extracted.ok) return null;

  const registry = env.DEVICE_REGISTRY_DO.get(env.DEVICE_REGISTRY_DO.idFromName("devices-v1"));
  const deviceAuth = await authenticateDeviceCredential(registry, workstationId, extracted.credential);
  if (deviceAuth.ok) return `device:${deviceAuth.device_id}`;
  if (deviceAuth.code !== "device_not_found" && deviceAuth.code !== "device_credential_missing") {
    return null;
  }
  if (!env.DEFAULT_WORKSTATION_ID || workstationId !== env.DEFAULT_WORKSTATION_ID) return null;
  // Pre-device-registry single-workstation installs keep the legacy shared
  // secret fallback only for the configured default workstation. Once a
  // device record exists, the per-device credential is authoritative.
  const legacy = new SharedSecretLinkAuthenticator({ secret: env.LINK_SHARED_SECRET });
  return legacy.authenticate(request, workstationId, Date.now()).ok
    ? `legacy-link:${workstationId}`
    : null;
}

async function oauthInternal(env: Env, path: string, body: Record<string, unknown>): Promise<Response> {
  const stub = env.OAUTH_STORE_DO.get(env.OAUTH_STORE_DO.idFromName("oauth-v1"));
  return stub.fetch(new Request(`https://oauth.internal${path}`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  }));
}

async function createAutomationClient(
  env: Env,
  name: string,
  createdBy: string,
  deviceId: string,
  deviceName: string | null,
): Promise<
  | { ok: true; action: "automation_create"; client_id: string; client_secret: string; name: string; device_id: string; device_name: string | null; token_endpoint: string; scope: "mcp" }
  | { ok: false; code: string }
> {
  if (!env.OAUTH_ISSUER) return { ok: false, code: "oauth_not_configured" };
  const identity = createOAuthIdentity(env.OAUTH_ISSUER);
  const clientId = `svc_${randomBase64UrlToken().slice(0, 22)}`;
  const clientSecret = `herdr_svc_${randomBase64UrlToken()}`;
  const response = await oauthInternal(env, "/internal/oauth/automation/create", {
    client_id: clientId,
    client_secret_hash: await sha256Hex(clientSecret),
    client_name: name,
    resource: identity.resource,
    scope: "mcp",
    created_by: createdBy,
    device_id: deviceId,
    device_name: deviceName,
    now_ms: Date.now(),
  });
  if (!response.ok) {
    const payload = await response.json().catch(() => null) as { code?: string } | null;
    return { ok: false, code: payload?.code ?? "automation_create_failed" };
  }
  return {
    ok: true,
    action: "automation_create",
    client_id: clientId,
    client_secret: clientSecret,
    name,
    device_id: deviceId,
    device_name: deviceName,
    token_endpoint: `${identity.issuer}/oauth/token`,
    scope: "mcp",
  };
}

async function listAutomationClients(
  env: Env,
): Promise<{ ok: true; automations: unknown[] } | { ok: false; code: string }> {
  const response = await oauthInternal(env, "/internal/oauth/automation/list", {});
  if (!response.ok) return { ok: false, code: "automation_list_failed" };
  const payload = await response.json().catch(() => null) as { automations?: unknown[] } | null;
  return { ok: true, automations: Array.isArray(payload?.automations) ? payload.automations : [] };
}

async function rotateAutomationClient(
  env: Env,
  clientId: string,
  rotatedBy: string,
): Promise<
  | { ok: true; action: "automation_rotate"; client_id: string; client_secret: string }
  | { ok: false; code: string }
> {
  const clientSecret = `herdr_svc_${randomBase64UrlToken()}`;
  const response = await oauthInternal(env, "/internal/oauth/automation/rotate", {
    client_id: clientId,
    client_secret_hash: await sha256Hex(clientSecret),
    rotated_by: rotatedBy,
    now_ms: Date.now(),
  });
  if (!response.ok) {
    const payload = await response.json().catch(() => null) as { code?: string } | null;
    return { ok: false, code: payload?.code ?? "automation_rotate_failed" };
  }
  return { ok: true, action: "automation_rotate", client_id: clientId, client_secret: clientSecret };
}

async function revokeAutomationClient(
  env: Env,
  clientId: string,
  revokedBy: string,
): Promise<{ ok: true } | { ok: false; code: string }> {
  const stub = env.OAUTH_STORE_DO.get(env.OAUTH_STORE_DO.idFromName("oauth-v1"));
  const store = createOAuthPublicStore(stub);
  const grant = await store.getGrant(clientId);
  if (!grant) return { ok: false, code: "automation_client_not_found" };
  if (grant.principal_type !== "automation") return { ok: false, code: "not_automation_client" };
  if (!(await store.revokeGrant(clientId, revokedBy, Date.now()))) {
    return { ok: false, code: "automation_revoke_failed" };
  }
  return { ok: true };
}

async function approveConnectorRequest(
  env: Env,
  requestId: string,
  code: string,
  approver: string,
): Promise<{ ok: true; client_id: string; approved_at_ms: number | null } | { ok: false; code: string }> {
  if (!env.LINK_SHARED_SECRET) return { ok: false, code: "connector_approval_not_configured" };
  const stub = env.OAUTH_STORE_DO.get(env.OAUTH_STORE_DO.idFromName("oauth-v1"));
  const store = createOAuthPublicStore(stub);
  const result = await store.approveApproval(
    requestId,
    await hashOAuthApprovalCode(env.LINK_SHARED_SECRET, requestId, code),
    approver,
    Date.now(),
  );
  if (!result.ok) return { ok: false, code: result.code };
  return {
    ok: true,
    client_id: result.record.client_id,
    approved_at_ms: result.record.approved_at_ms ?? null,
  };
}

async function inspectConnectorRequest(
  env: Env,
  requestId: string,
): Promise<
  | {
      ok: true;
      request_id: string;
      client_id: string;
      client_name: string | null;
      redirect_uri: string;
      resource: string;
      scope: string;
      status: string;
      expires_at_ms: number;
    }
  | { ok: false; code: string }
> {
  const stub = env.OAUTH_STORE_DO.get(env.OAUTH_STORE_DO.idFromName("oauth-v1"));
  const store = createOAuthPublicStore(stub);
  const approval = await store.getApproval(requestId, Date.now());
  if (!approval) return { ok: false, code: "connector_approval_not_found" };
  const client = await store.getClient(approval.client_id);
  return {
    ok: true,
    request_id: requestId,
    client_id: approval.client_id,
    client_name: client?.client_name ?? null,
    redirect_uri: approval.redirect_uri,
    resource: approval.resource,
    scope: approval.scope,
    status: approval.status,
    expires_at_ms: approval.expires_at_ms,
  };
}

async function revokeConnectorGrant(
  env: Env,
  clientId: string,
  revokedBy: string,
): Promise<{ ok: true } | { ok: false; code: string }> {
  const stub = env.OAUTH_STORE_DO.get(env.OAUTH_STORE_DO.idFromName("oauth-v1"));
  const store = createOAuthPublicStore(stub);
  const grant = await store.getGrant(clientId);
  if (!grant) return { ok: false, code: "connector_grant_not_found" };
  if (!(await store.revokeGrant(clientId, revokedBy, Date.now()))) return { ok: false, code: "connector_revoke_failed" };
  return { ok: true };
}