import { isRoutableDevice, normalizeDeviceId, type DeviceRecord } from "./device-model.js";

interface FetchStub {
  fetch(request: Request): Promise<Response>;
}

export async function ensureLegacyDeviceRegistration(
  registry: FetchStub,
  workstationId: string,
): Promise<{ device_id: string; created: boolean }> {
  const response = await registry.fetch(new Request("https://registry.internal/internal/devices/legacy/ensure", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ workstation_id: workstationId, name: workstationId }),
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
      workstation_id: string;
      routing_reason: "explicit_device" | "device_ref" | "single_available_device" | "legacy_default_device";
    }
  | {
      ok: false;
      code: "device_not_found" | "device_ambiguous" | "device_ref_conflict" | "device_paused" | "device_suspended" | "device_revoked" | "device_unavailable";
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
  if (device.authorization === "revoked") return { ok: false, code: "device_revoked" };
  if (device.authorization === "suspended") return { ok: false, code: "device_suspended" };
  if (device.scheduling !== "enabled") return { ok: false, code: "device_paused" };
  return { ok: false, code: "device_unavailable" };
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
      if (extractedRef.deviceId === "__conflict__" || extractedRef.deviceId === "__type_mismatch__") {
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
      : devices.filter((device) => device.name === trimmed);
    if (matches.length === 0) return { ok: false, code: "device_not_found" };
    if (matches.length > 1) return { ok: false, code: "device_ambiguous" };
    const selected = matches[0];
    if (!isRoutableDevice(selected)) return unavailableReason(selected);
    if (extractedRef && extractedRef.deviceId !== selected.device_id) {
      return { ok: false, code: "device_ref_conflict" };
    }
    return {
      ok: true,
      device_id: selected.device_id,
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
      workstation_id: device.workstation_id,
      routing_reason: "device_ref",
    };
  }

  const routable = devices.filter(isRoutableDevice);
  if (routable.length === 1) {
    return {
      ok: true,
      device_id: routable[0].device_id,
      workstation_id: routable[0].workstation_id,
      routing_reason: "single_available_device",
    };
  }
  if (routable.length > 1) return { ok: false, code: "device_ambiguous" };
  if (devices.length === 1) return unavailableReason(devices[0]);
  if (devices.length > 1) return { ok: false, code: "device_unavailable" };
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
  const devices = await readRegistryDevices(registry);
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
