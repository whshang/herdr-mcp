import type { Env } from "./env.js";
import { constantTimeEqual } from "./auth.js";
import {
  isCredentialVerifier,
  isEnrollmentCode,
  newCredentialId,
  newDeviceSecret,
  newEnrollmentCode,
  sha256Hex,
} from "./device-crypto.js";
import {
  isWorkstationId,
  newDeviceId,
  normalizeDeviceId,
  validateDeviceRecord,
  type DeviceRecord,
} from "./device-model.js";

const DEVICE_PREFIX = "device:";
const WORKSTATION_PREFIX = "workstation:";
const ENROLLMENT_PREFIX = "enrollment:";
const CREDENTIAL_PREFIX = "credential:";
const DEFAULT_ENROLLMENT_TTL_MS = 10 * 60 * 1000;
const MIN_ENROLLMENT_TTL_MS = 60 * 1000;
const MAX_ENROLLMENT_TTL_MS = 15 * 60 * 1000;
const enc = new TextEncoder();

interface EnrollmentRecord {
  verifier_sha256: string;
  created_at_ms: number;
  expires_at_ms: number;
  name: string | null;
}

interface DeviceCredentialRecord {
  credential_id: string;
  device_id: string;
  workstation_id: string;
  verifier_sha256: string;
  created_at_ms: number;
}

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
    if (request.method === "POST" && url.pathname === "/internal/devices/enrollments") {
      return this.createEnrollment(request);
    }
    if (request.method === "POST" && url.pathname === "/internal/devices/enrollments/consume") {
      return this.consumeEnrollment(request);
    }
    if (request.method === "POST" && url.pathname === "/internal/devices/authenticate") {
      return this.authenticateDevice(request);
    }
    const revokeMatch = /^\/internal\/devices\/(dev_[0-9A-Z]+)\/revoke$/.exec(url.pathname);
    if (request.method === "POST" && revokeMatch) {
      return this.revokeDevice(revokeMatch[1]);
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

  private async createEnrollment(request: Request): Promise<Response> {
    let body: unknown;
    try {
      body = await request.json();
    } catch {
      return json({ ok: false, code: "bad_request" }, 400);
    }
    if (!isRecord(body)) return json({ ok: false, code: "bad_request" }, 400);
    const ttlMs = normalizeEnrollmentTtlMs(body.ttl_seconds);
    if (ttlMs === null) return json({ ok: false, code: "invalid_enrollment_ttl" }, 400);
    const name = normalizeOptionalName(body.name);
    if (body.name !== undefined && name === null) return json({ ok: false, code: "invalid_device_name" }, 400);

    const enrollmentCode = newEnrollmentCode();
    const verifier = await sha256Hex(enrollmentCode);
    const now = Date.now();
    const record: EnrollmentRecord = {
      verifier_sha256: verifier,
      created_at_ms: now,
      expires_at_ms: now + ttlMs,
      name,
    };
    await this.state.storage.put(ENROLLMENT_PREFIX + verifier, record);
    return json({ ok: true, enrollment_code: enrollmentCode, expires_at_ms: record.expires_at_ms });
  }

  private async consumeEnrollment(request: Request): Promise<Response> {
    let body: unknown;
    try {
      body = await request.json();
    } catch {
      return json({ ok: false, code: "bad_request" }, 400);
    }
    if (!isRecord(body) || !isEnrollmentCode(body.enrollment_code)) {
      return json({ ok: false, code: "invalid_enrollment" }, 401);
    }
    const requestedName = normalizeOptionalName(body.name);
    if (body.name !== undefined && requestedName === null) return json({ ok: false, code: "invalid_device_name" }, 400);

    const verifier = await sha256Hex(body.enrollment_code);
    const now = Date.now();
    const deviceId = newDeviceId(now);
    const workstationId = deviceId;
    const credentialId = newCredentialId();
    const deviceSecret = newDeviceSecret();
    const credentialVerifier = await sha256Hex(deviceSecret);

    const result = await this.state.storage.transaction(async (tx) => {
      const enrollmentKey = ENROLLMENT_PREFIX + verifier;
      const enrollment = await tx.get<EnrollmentRecord>(enrollmentKey);
      if (!enrollment || enrollment.verifier_sha256 !== verifier) {
        return { ok: false as const, code: "invalid_enrollment" };
      }
      if (enrollment.expires_at_ms <= now) {
        await tx.delete(enrollmentKey);
        return { ok: false as const, code: "enrollment_expired" };
      }
      if (await tx.get(WORKSTATION_PREFIX + workstationId)) {
        return { ok: false as const, code: "registry_conflict" };
      }

      const device: DeviceRecord = {
        device_id: deviceId,
        workstation_id: workstationId,
        name: enrollment.name ?? requestedName ?? deviceId,
        authorization: "active",
        scheduling: "enabled",
        credential_id: credentialId,
        enrolled_at_ms: now,
        updated_at_ms: now,
        revoked_at_ms: null,
      };
      const credential: DeviceCredentialRecord = {
        credential_id: credentialId,
        device_id: deviceId,
        workstation_id: workstationId,
        verifier_sha256: credentialVerifier,
        created_at_ms: now,
      };
      await tx.delete(enrollmentKey);
      await tx.put(DEVICE_PREFIX + deviceId, device);
      await tx.put(WORKSTATION_PREFIX + workstationId, deviceId);
      await tx.put(CREDENTIAL_PREFIX + credentialId, credential);
      return { ok: true as const, device };
    });

    if (!result.ok) {
      const status = result.code === "enrollment_expired" ? 410 : result.code === "invalid_enrollment" ? 401 : 409;
      return json(result, status);
    }
    return json({
      ok: true,
      device_id: result.device.device_id,
      workstation_id: result.device.workstation_id,
      credential_id: credentialId,
      device_secret: deviceSecret,
    });
  }

  private async authenticateDevice(request: Request): Promise<Response> {
    let body: unknown;
    try {
      body = await request.json();
    } catch {
      return json({ ok: false, code: "bad_request" }, 400);
    }
    if (!isRecord(body) || !isWorkstationId(body.workstation_id) || !isCredentialVerifier(body.credential_verifier_sha256)) {
      return json({ ok: false, code: "bad_request" }, 400);
    }
    const deviceId = await this.state.storage.get<string>(WORKSTATION_PREFIX + body.workstation_id);
    if (!deviceId) return json({ ok: false, code: "device_not_found" }, 401);
    const device = parseDeviceRecord(await this.state.storage.get<DeviceRecord>(DEVICE_PREFIX + deviceId));
    if (!device) return json({ ok: false, code: "registry_corrupt" }, 409);
    if (device.authorization === "revoked") return json({ ok: false, code: "device_revoked" }, 403);
    if (device.authorization === "suspended") return json({ ok: false, code: "device_suspended" }, 403);
    if (!device.credential_id) return json({ ok: false, code: "device_credential_missing" }, 401);

    const credential = await this.state.storage.get<DeviceCredentialRecord>(CREDENTIAL_PREFIX + device.credential_id);
    if (!credential || credential.device_id !== device.device_id || credential.workstation_id !== device.workstation_id) {
      return json({ ok: false, code: "registry_corrupt" }, 409);
    }
    if (!constantTimeEqual(enc.encode(credential.verifier_sha256), enc.encode(body.credential_verifier_sha256))) {
      return json({ ok: false, code: "link_auth_failed" }, 401);
    }
    return json({ ok: true, device_id: device.device_id, credential_id: credential.credential_id });
  }

  private async revokeDevice(deviceId: string): Promise<Response> {
    const canonical = normalizeDeviceId(deviceId);
    if (!canonical) return json({ ok: false, code: "invalid_device_id" }, 400);
    const existing = parseDeviceRecord(await this.state.storage.get<DeviceRecord>(DEVICE_PREFIX + canonical));
    if (!existing) return json({ ok: false, code: "device_not_found" }, 404);
    const now = Date.now();
    const revoked: DeviceRecord = {
      ...existing,
      authorization: "revoked",
      revoked_at_ms: existing.revoked_at_ms ?? now,
      updated_at_ms: now,
    };
    await this.state.storage.put(DEVICE_PREFIX + canonical, revoked);
    return json({ ok: true, device: revoked });
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

function normalizeEnrollmentTtlMs(value: unknown): number | null {
  if (value === undefined) return DEFAULT_ENROLLMENT_TTL_MS;
  if (typeof value !== "number" || !Number.isSafeInteger(value)) return null;
  const ttlMs = value * 1000;
  return ttlMs >= MIN_ENROLLMENT_TTL_MS && ttlMs <= MAX_ENROLLMENT_TTL_MS ? ttlMs : null;
}

function normalizeOptionalName(value: unknown): string | null {
  if (value === undefined || value === null) return null;
  if (typeof value !== "string") return null;
  const name = value.trim();
  return name.length > 0 && name.length <= 128 ? name : null;
}
