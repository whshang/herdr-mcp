import { sha256Hex } from "./device-crypto.js";
import { isRoutableDevice, normalizeDeviceId, type DeviceRecord } from "./device-model.js";

interface FetchStub {
  fetch(request: Request): Promise<Response>;
}

export interface PairingSession {
  pairing_id: string;
  code: string;
  expires_at_ms: number;
}

export interface PairedDeviceCredential {
  device_id: string;
  workstation_id: string;
  credential_id: string;
  device_secret: string;
}

export type DeviceCredentialAuthCode =
  | "device_not_found"
  | "device_credential_missing"
  | "device_revoked"
  | "device_suspended"
  | "link_auth_failed"
  | "registry_corrupt"
  | "internal_error";

export type DeviceCredentialAuthResult =
  | { ok: true; device_id: string; credential_id: string }
  | { ok: false; code: DeviceCredentialAuthCode };

export async function createPairingSession(
  registry: FetchStub,
  input: { ttl_seconds?: number; name?: string; worker_context: string },
): Promise<{ ok: true; pairing: PairingSession } | { ok: false; code: string; status: number }> {
  const response = await registry.fetch(new Request("https://registry.internal/internal/devices/pairings", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(input),
  }));
  const body: unknown = await response.json();
  if (!response.ok) {
    return {
      ok: false,
      code: isRecord(body) && typeof body.code === "string" ? body.code : "pairing_create_failed",
      status: response.status,
    };
  }
  if (!isRecord(body) || body.ok !== true || typeof body.pairing_id !== "string" || typeof body.code !== "string" || typeof body.expires_at_ms !== "number") {
    return { ok: false, code: "invalid_registry_response", status: 503 };
  }
  return {
    ok: true,
    pairing: { pairing_id: body.pairing_id, code: body.code, expires_at_ms: body.expires_at_ms },
  };
}

export async function consumePairingSession(
  registry: FetchStub,
  input: { pairing_id: string; code: string; name?: string; worker_context: string },
): Promise<{ ok: true; credential: PairedDeviceCredential } | { ok: false; code: string; status: number }> {
  const response = await registry.fetch(new Request("https://registry.internal/internal/devices/pairings/consume", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(input),
  }));
  const body: unknown = await response.json();
  if (!response.ok) {
    return {
      ok: false,
      code: isRecord(body) && typeof body.code === "string" ? body.code : "pairing_rejected",
      status: response.status,
    };
  }
  if (!isRecord(body) || body.ok !== true || typeof body.device_id !== "string" || typeof body.workstation_id !== "string" || typeof body.credential_id !== "string" || typeof body.device_secret !== "string") {
    return { ok: false, code: "invalid_registry_response", status: 503 };
  }
  return {
    ok: true,
    credential: {
      device_id: body.device_id,
      workstation_id: body.workstation_id,
      credential_id: body.credential_id,
      device_secret: body.device_secret,
    },
  };
}

export async function authenticateDeviceCredential(
  registry: FetchStub,
  workstationId: string,
  credential: string,
): Promise<DeviceCredentialAuthResult> {
  try {
    const verifier = await sha256Hex(credential);
    const response = await registry.fetch(new Request("https://registry.internal/internal/devices/authenticate", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ workstation_id: workstationId, credential_verifier_sha256: verifier }),
    }));
    const body: unknown = await response.json();
    if (response.ok && isRecord(body) && body.ok === true && typeof body.device_id === "string" && typeof body.credential_id === "string") {
      return { ok: true, device_id: body.device_id, credential_id: body.credential_id };
    }
    const code = isRecord(body) && typeof body.code === "string" ? body.code : "internal_error";
    switch (code) {
      case "device_not_found":
      case "device_credential_missing":
      case "device_revoked":
      case "device_suspended":
      case "link_auth_failed":
      case "registry_corrupt":
        return { ok: false, code };
      default:
        return { ok: false, code: "internal_error" };
    }
  } catch {
    return { ok: false, code: "internal_error" };
  }
}

export type RenameDeviceResult =
  | { ok: true; device_id: string; name: string; updated_at_ms: number; wrote_registry: boolean }
  | { ok: false; code: "device_not_found" | "device_revoked" | "invalid_device_name" | "registry_corrupt" | "internal_error" };

export async function renameRegisteredDevice(
  registry: FetchStub,
  workstationId: string,
  name: string,
): Promise<RenameDeviceResult> {
  try {
    const response = await registry.fetch(new Request("https://registry.internal/internal/devices/rename", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ workstation_id: workstationId, name }),
    }));
    const body: unknown = await response.json();
    if (
      response.ok &&
      isRecord(body) &&
      body.ok === true &&
      typeof body.device_id === "string" &&
      typeof body.name === "string" &&
      typeof body.updated_at_ms === "number"
    ) {
      return {
        ok: true,
        device_id: body.device_id,
        name: body.name,
        updated_at_ms: body.updated_at_ms,
        wrote_registry: body.wrote_registry === true,
      };
    }
    const code = isRecord(body) && typeof body.code === "string" ? body.code : "internal_error";
    switch (code) {
      case "device_not_found":
      case "device_revoked":
      case "invalid_device_name":
      case "registry_corrupt":
        return { ok: false, code };
      default:
        return { ok: false, code: "internal_error" };
    }
  } catch {
    return { ok: false, code: "internal_error" };
  }
}

export type RevokeDeviceResult =
  | { ok: true; device_id: string; revoked: true; revoked_at_ms: number; wrote_registry: boolean }
  | { ok: false; code: "device_not_found" | "invalid_device_id" | "revoke_teardown_failed" | "internal_error"; retryable: boolean };

/**
 * Revoke an enrolled device by immutable device_id. The registry DO persists
 * the revoked authorization first (fail-closed), then tears down the target
 * device's live WorkstationDO/WebSocket session. Idempotent: a repeated revoke
 * performs no additional registry write but still re-issues teardown.
 */
export async function revokeRegisteredDevice(registry: FetchStub, deviceId: string): Promise<RevokeDeviceResult> {
  try {
    const response = await registry.fetch(new Request(
      `https://registry.internal/internal/devices/${encodeURIComponent(deviceId)}/revoke`,
      { method: "POST" },
    ));
    const body: unknown = await response.json();
    if (response.ok && isRecord(body) && body.ok === true && typeof body.device_id === "string") {
      return {
        ok: true,
        device_id: body.device_id,
        revoked: true,
        revoked_at_ms: typeof body.revoked_at_ms === "number" ? body.revoked_at_ms : Date.now(),
        wrote_registry: body.wrote_registry === true,
      };
    }
    if (isRecord(body) && typeof body.code === "string") {
      if (body.code === "device_not_found") return { ok: false, code: "device_not_found", retryable: false };
      if (body.code === "invalid_device_id") return { ok: false, code: "invalid_device_id", retryable: false };
      if (body.code === "revoke_teardown_failed") return { ok: false, code: "revoke_teardown_failed", retryable: true };
    }
    return { ok: false, code: "internal_error", retryable: false };
  } catch {
    return { ok: false, code: "internal_error", retryable: false };
  }
}

export async function ensureLegacyDeviceRegistration(
  registry: FetchStub,
  workstationId: string,
  name?: string,
): Promise<{ device_id: string; created: boolean }> {
  const response = await registry.fetch(new Request("https://registry.internal/internal/devices/legacy/ensure", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ workstation_id: workstationId, name: name ?? workstationId }),
  }));
  if (!response.ok) throw new Error(`legacy device registration returned HTTP ${response.status}`);
  const body: unknown = await response.json();
  if (!isRecord(body) || body.ok !== true || !isRecord(body.device) || typeof body.device.device_id !== "string") {
    throw new Error("legacy device registration returned an invalid response");
  }
  return { device_id: body.device.device_id, created: body.created === true };
}

export interface PublicDeviceSummary {
  device_id: string;
  name: string;
  authorization: DeviceRecord["authorization"];
  scheduling: DeviceRecord["scheduling"];
  connection: "online" | "stale" | "offline";
  health: string;
  runtime_version: string | null;
  runtime_generation: string | null;
  last_seen_ago_ms: number | null;
  active_requests: number;
}

interface RegistryEnvelope {
  ok?: boolean;
  devices?: DeviceRecord[];
}

export type DeviceRouteResult =
  | {
      ok: true;
      device_id: string | null;
      device_name?: string;
      workstation_id: string;
      routing_reason: "explicit_device" | "device_ref" | "single_available_device" | "legacy_default_device";
    }
  | {
      ok: false;
      code: "device_not_found" | "device_ambiguous" | "device_ref_conflict" | "device_paused" | "device_suspended" | "device_revoked" | "device_unavailable";
      selected_device?: { device_id: string; name: string };
      candidate_devices?: Array<{ device_id: string; name: string }>;
    };

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

async function readStatus(stub: FetchStub): Promise<Record<string, unknown> | null> {
  try {
    const response = await stub.fetch(new Request("https://workstation.internal/internal/status"));
    if (!response.ok) return null;
    const body: unknown = await response.json();
    return isRecord(body) ? body : null;
  } catch {
    return null;
  }
}

function connectionFromStatus(status: Record<string, unknown> | null): PublicDeviceSummary["connection"] {
  if (status?.online === true) return "online";
  if (status?.connected === true) return "stale";
  return "offline";
}

function nonNegativeInteger(value: unknown): number {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0 ? value : 0;
}

async function readRegistryDevices(registry: FetchStub): Promise<DeviceRecord[]> {
  const response = await registry.fetch(new Request("https://registry.internal/internal/devices"));
  if (!response.ok) throw new Error(`device registry returned HTTP ${response.status}`);
  const envelope = await response.json() as RegistryEnvelope;
  if (envelope.ok !== true || !Array.isArray(envelope.devices)) {
    throw new Error("device registry returned an invalid device list");
  }
  return envelope.devices;
}

function unavailableReason(device: DeviceRecord): DeviceRouteResult {
  const selected_device = { device_id: device.device_id, name: device.name };
  if (device.authorization === "revoked") return { ok: false, code: "device_revoked", selected_device };
  if (device.authorization === "suspended") return { ok: false, code: "device_suspended", selected_device };
  if (device.scheduling !== "enabled") return { ok: false, code: "device_paused", selected_device };
  return { ok: false, code: "device_unavailable", selected_device };
}

export async function resolveDeviceRoute(
  registry: FetchStub,
  selector: string | undefined,
  legacyWorkstationId: string,
): Promise<DeviceRouteResult> {
  return resolveDeviceRouteWithContext(registry, { selector, legacyWorkstationId });
}

export interface DeviceRouteContext {
  selector?: string;
  args?: Record<string, unknown>;
  legacyWorkstationId: string;
}

export async function resolveDeviceRouteWithContext(
  registry: FetchStub,
  ctx: DeviceRouteContext,
): Promise<DeviceRouteResult> {
  const devices = await readRegistryDevices(registry);
  const byId = new Map(devices.map((d) => [d.device_id, d]));

  // Pre-extract opaque ref for explicit-vs-ref conflict detection.
  // Binding args are NOT trusted (caller-controlled) – they are ignored for routing.
  let extractedRef: { deviceId: string; field: string; raw: string; kind: "ref" } | null = null;
  if (ctx.args) {
    const { extractDeviceIdFromArgs } = await import("./device-refs.js");
    extractedRef = extractDeviceIdFromArgs(ctx.args);
    if (extractedRef) {
      if (extractedRef.deviceId === "__conflict__" || extractedRef.deviceId === "__type_mismatch__" || extractedRef.deviceId === "__malformed__") {
        return { ok: false, code: "device_ref_conflict" };
      }
    }
  }

  // 1) explicit device selector (highest priority) – but fail closed if it
  // conflicts with an opaque ref bound to a different device. Caller must use
  // raw B-local ids with device=B, not reuse A's opaque ref on B.
  if (ctx.selector !== undefined) {
    const trimmed = ctx.selector.trim();
    if (trimmed.length === 0) return { ok: false, code: "device_not_found" };
    const canonicalId = normalizeDeviceId(trimmed);
    const matches = canonicalId
      ? devices.filter((device) => device.device_id === canonicalId)
      : devices.filter((device) => device.name === trimmed || device.aliases?.includes(trimmed) === true);
    if (matches.length === 0) return { ok: false, code: "device_not_found" };
    if (matches.length > 1) {
      return {
        ok: false,
        code: "device_ambiguous",
        candidate_devices: matches.map((device) => ({ device_id: device.device_id, name: device.name })),
      };
    }
    const selected = matches[0];
    if (!isRoutableDevice(selected)) return unavailableReason(selected);
    if (extractedRef && extractedRef.deviceId !== selected.device_id) {
      return { ok: false, code: "device_ref_conflict" };
    }
    return {
      ok: true,
      device_id: selected.device_id,
      device_name: selected.name,
      workstation_id: selected.workstation_id,
      routing_reason: "explicit_device",
    };
  }

  // 2) device-aware opaque ref (workspace/pane) – strict, no binding fallback
  if (extractedRef) {
    const device = byId.get(extractedRef.deviceId);
    if (!device) return { ok: false, code: "device_not_found" };
    if (!isRoutableDevice(device)) return unavailableReason(device);
    return {
      ok: true,
      device_id: device.device_id,
      device_name: device.name,
      workstation_id: device.workstation_id,
      routing_reason: "device_ref",
    };
  }

  // 3) Backward-compatible implicit routing. Before multi-device support, an
  // omitted selector always meant the configured legacy/default workstation.
  // Preserve that meaning when the registry contains exactly one record bound
  // to that workstation_id. Never silently fail over to a different machine:
  // an unavailable default reports its own state, and corrupt duplicate legacy
  // mappings remain fail-closed.
  const legacyMatches = devices.filter((device) => device.workstation_id === ctx.legacyWorkstationId);
  if (legacyMatches.length > 1) {
    return {
      ok: false,
      code: "device_ambiguous",
      candidate_devices: legacyMatches.map((device) => ({ device_id: device.device_id, name: device.name })),
    };
  }
  if (legacyMatches.length === 1) {
    const selected = legacyMatches[0];
    if (!isRoutableDevice(selected)) return unavailableReason(selected);
    return {
      ok: true,
      device_id: selected.device_id,
      device_name: selected.name,
      workstation_id: selected.workstation_id,
      routing_reason: "legacy_default_device",
    };
  }

  const routable = devices.filter(isRoutableDevice);
  if (routable.length === 1) {
    return {
      ok: true,
      device_id: routable[0].device_id,
      device_name: routable[0].name,
      workstation_id: routable[0].workstation_id,
      routing_reason: "single_available_device",
    };
  }
  if (routable.length > 1) {
    return {
      ok: false,
      code: "device_ambiguous",
      candidate_devices: routable.map((device) => ({ device_id: device.device_id, name: device.name })),
    };
  }
  if (devices.length === 1) return unavailableReason(devices[0]);
  if (devices.length > 1) {
    return {
      ok: false,
      code: "device_unavailable",
      candidate_devices: devices.map((device) => ({ device_id: device.device_id, name: device.name })),
    };
  }
  return {
    ok: true,
    device_id: null,
    workstation_id: ctx.legacyWorkstationId,
    routing_reason: "legacy_default_device",
  };
}

export async function listPublicDevices(
  registry: FetchStub,
  getWorkstationStub: (workstationId: string) => FetchStub,
): Promise<PublicDeviceSummary[]> {
  // Revoked records are durable authorization tombstones, not fleet members.
  // Keep them in DeviceRegistryDO so old credentials can never resurrect, but
  // omit them from normal inventory surfaces and avoid stale status lookups.
  const devices = (await readRegistryDevices(registry))
    .filter((device) => device.authorization !== "revoked");
  return Promise.all(devices.map(async (device) => {
    const status = await readStatus(getWorkstationStub(device.workstation_id));
    return {
      device_id: device.device_id,
      name: device.name,
      authorization: device.authorization,
      scheduling: device.scheduling,
      connection: connectionFromStatus(status),
      health: typeof status?.runtimeHealth === "string" ? status.runtimeHealth : "unknown",
      runtime_version: typeof status?.runtimeVersion === "string" ? status.runtimeVersion : null,
      runtime_generation: typeof status?.runtimeGeneration === "string" ? status.runtimeGeneration : null,
      last_seen_ago_ms: typeof status?.lastSeenAgoMs === "number" && status.lastSeenAgoMs >= 0 ? status.lastSeenAgoMs : null,
      active_requests: nonNegativeInteger(status?.activeRequests),
    } satisfies PublicDeviceSummary;
  }));
}
