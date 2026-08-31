import type { DeviceRecord } from "./device-model.js";

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

export async function listPublicDevices(
  registry: FetchStub,
  getWorkstationStub: (workstationId: string) => FetchStub,
): Promise<PublicDeviceSummary[]> {
  const response = await registry.fetch(new Request("https://registry.internal/internal/devices"));
  if (!response.ok) throw new Error(`device registry returned HTTP ${response.status}`);
  const envelope = await response.json() as RegistryEnvelope;
  if (envelope.ok !== true || !Array.isArray(envelope.devices)) {
    throw new Error("device registry returned an invalid device list");
  }

  return Promise.all(envelope.devices.map(async (device) => {
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
