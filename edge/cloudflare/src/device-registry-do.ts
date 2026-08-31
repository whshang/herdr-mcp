import type { Env } from "./env.js";
import {
  normalizeDeviceId,
  validateDeviceRecord,
  type DeviceRecord,
} from "./device-model.js";

const DEVICE_PREFIX = "device:";

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
}
