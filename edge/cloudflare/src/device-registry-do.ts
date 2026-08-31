import type { Env } from "./env.js";
import {
  isWorkstationId,
  newDeviceId,
  normalizeDeviceId,
  validateDeviceRecord,
  type DeviceRecord,
} from "./device-model.js";

const DEVICE_PREFIX = "device:";
const WORKSTATION_PREFIX = "workstation:";

function json(payload: unknown, status = 200): Response {
  return new Response(JSON.stringify(payload), {
    status,
    headers: { "content-type": "application/json", "cache-control": "no-store" },
  });
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function parseDeviceRecord(value: unknown): DeviceRecord | null {
  if (!isRecord(value)) return null;
  const candidate = value as unknown as DeviceRecord;
  return validateDeviceRecord(candidate) === null ? candidate : null;
}

export class DeviceRegistryDO {
  constructor(private readonly state: DurableObjectState, private readonly env: Env) {
    void this.env;
  }

  async fetch(request: Request): Promise<Response> {
    const url = new URL(request.url);
    if (!url.pathname.startsWith("/internal/devices")) return json({ ok: false, code: "not_found" }, 404);

    if (request.method === "GET" && url.pathname === "/internal/devices") {
      return this.listDevices();
    }
    if (request.method === "POST" && url.pathname === "/internal/devices/legacy/ensure") {
      return this.ensureLegacyDevice(request);
    }

    const match = /^\/internal\/devices\/([^/]+)$/.exec(url.pathname);
    if (!match) return json({ ok: false, code: "not_found" }, 404);
    const decoded = decodeURIComponent(match[1] ?? "");
    const deviceId = normalizeDeviceId(decoded);
    if (deviceId === null || deviceId !== decoded) {
      return json({ ok: false, code: "invalid_device_id" }, 400);
    }

    if (request.method === "GET") return this.getDevice(deviceId);
    if (request.method === "PUT") return this.putDevice(deviceId, request);
    return json({ ok: false, code: "method_not_allowed" }, 405);
  }

  private async listDevices(): Promise<Response> {
    const stored = await this.state.storage.list<DeviceRecord>({ prefix: DEVICE_PREFIX });
    const devices: DeviceRecord[] = [];
    for (const value of stored.values()) {
      const parsed = parseDeviceRecord(value);
      if (parsed) devices.push(parsed);
    }
    devices.sort((a, b) => a.device_id.localeCompare(b.device_id));
    return json({ ok: true, devices });
  }

  private async getDevice(deviceId: string): Promise<Response> {
    const value = await this.state.storage.get<DeviceRecord>(DEVICE_PREFIX + deviceId);
    const parsed = parseDeviceRecord(value);
    return parsed ? json({ ok: true, device: parsed }) : json({ ok: false, code: "device_not_found" }, 404);
  }

  private async putDevice(deviceId: string, request: Request): Promise<Response> {
    let body: unknown;
    try {
      body = await request.json();
    } catch {
      return json({ ok: false, code: "bad_request" }, 400);
    }
    const parsed = parseDeviceRecord(body);
    if (!parsed) return json({ ok: false, code: "invalid_device_record" }, 400);
    if (parsed.device_id !== deviceId) return json({ ok: false, code: "device_id_mismatch" }, 409);
    await this.state.storage.put(DEVICE_PREFIX + deviceId, parsed);
    return json({ ok: true, device: parsed });
  }

  private async ensureLegacyDevice(request: Request): Promise<Response> {
    let body: unknown;
    try {
      body = await request.json();
    } catch {
      return json({ ok: false, code: "bad_request" }, 400);
    }
    if (!isRecord(body) || !isWorkstationId(body.workstation_id)) {
      return json({ ok: false, code: "invalid_workstation_id" }, 400);
    }
    const workstationId = body.workstation_id;
    const requestedName = typeof body.name === "string" ? body.name.trim() : "";
    const name = requestedName.length > 0 && requestedName.length <= 128 ? requestedName : workstationId;
    const now = Date.now();

    const result = await this.state.storage.transaction(async (tx) => {
      const indexKey = WORKSTATION_PREFIX + workstationId;
      const existingId = await tx.get<string>(indexKey);
      if (existingId) {
        const existing = await tx.get<DeviceRecord>(DEVICE_PREFIX + existingId);
        const parsed = parseDeviceRecord(existing);
        if (!parsed) return { ok: false as const, code: "registry_corrupt" };
        return { ok: true as const, created: false, device: parsed };
      }

      const deviceId = newDeviceId(now);
      const device: DeviceRecord = {
        device_id: deviceId,
        workstation_id: workstationId,
        name,
        authorization: "active",
        scheduling: "enabled",
        credential_id: null,
        enrolled_at_ms: now,
        updated_at_ms: now,
        revoked_at_ms: null,
      };
      await tx.put(DEVICE_PREFIX + deviceId, device);
      await tx.put(indexKey, deviceId);
      return { ok: true as const, created: true, device };
    });

    return result.ok ? json(result) : json(result, 409);
  }
}
