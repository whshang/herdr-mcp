import type { Env } from "./env.js";
import { constantTimeEqual } from "./auth.js";
import {
  isCredentialVerifier,
  isPairingCode,
  isPairingId,
  newCredentialId,
  newDeviceSecret,
  newPairingCode,
  newPairingId,
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
const PAIRING_PREFIX = "pairing:";
const CREDENTIAL_PREFIX = "credential:";
const DEFAULT_PAIRING_TTL_MS = 10 * 60 * 1000;
const MIN_PAIRING_TTL_MS = 60 * 1000;
const MAX_PAIRING_TTL_MS = 10 * 60 * 1000;
/** Fifth wrong code permanently locks the pairing session. */
const MAX_PAIRING_ATTEMPTS = 5;
const enc = new TextEncoder();

/**
 * Storage snapshot resistance: the lookup key is SHA-256(raw pairing_id), so a
 * snapshot alone never exposes the raw id. The stored verifier is
 * HMAC-SHA256 keyed by the Worker pepper (LINK_SHARED_SECRET, absent from DO
 * storage) over a domain-separated message "herdr-pairing-v1:" ||
 * pairing_id || ":" || worker_context || ":" || code, so a storage snapshot
 * + leaked pairing_id alone cannot enumerate the six-digit code offline.
 * Verified in constant time. The pepper is never stored in the DO, never
 * returned to clients, and never logged. Missing/invalid pepper fails closed.
 */
interface PairingRecord {
  verifier_sha256: string;
  created_at_ms: number;
  expires_at_ms: number;
  attempts: number;
  state: "pending" | "locked";
  name: string | null;
}

async function pairingStorageKey(pairingId: string): Promise<string> {
  return PAIRING_PREFIX + await sha256Hex(pairingId);
}

function getPairingPepper(env: Env): string | null {
  const candidate = env.LINK_SHARED_SECRET ?? null;
  if (
    candidate === null ||
    candidate.length === 0 ||
    candidate.length > 4096 ||
    [...candidate].some((ch) => ch.charCodeAt(0) < 0x20 || ch.charCodeAt(0) === 0x7f)
  ) {
    return null;
  }
  return candidate;
}

/**
 * HMAC-SHA256(key = pepper, message = "herdr-pairing-v1:" || pairing_id || ":" || worker_context || ":" || code).
 * Domain separation via fixed tag "herdr-pairing-v1" and colon separators.
 * pairing_id is fixed 69 chars (pair_ + 64 hex), code is 6 digits, worker_context is 1..256
 * validated without colons, so the encoding is unambiguous (lengths implicit). The pepper
 * is the Worker-side LINK_SHARED_SECRET and never leaves the Worker environment.
 */
async function pairingVerifier(
  pairingId: string,
  code: string,
  workerContext: string,
  pepper: string,
): Promise<string> {
  const key = await crypto.subtle.importKey(
    "raw",
    enc.encode(pepper),
    { name: "HMAC", hash: "SHA-256" },
    false,
    ["sign"],
  );
  const mac = await crypto.subtle.sign("HMAC", key, enc.encode(`herdr-pairing-v1:${pairingId}:${workerContext}:${code}`));
  return [...new Uint8Array(mac)].map((byte) => byte.toString(16).padStart(2, "0")).join("");
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
  constructor(private readonly state: DurableObjectState, private readonly env: Env) {}

  async fetch(request: Request): Promise<Response> {
    const url = new URL(request.url);
    if (!url.pathname.startsWith("/internal/devices")) return json({ ok: false, code: "not_found" }, 404);

    if (request.method === "GET" && url.pathname === "/internal/devices") {
      return this.listDevices();
    }
    if (request.method === "POST" && url.pathname === "/internal/devices/pairings") {
      return this.createPairing(request);
    }
    if (request.method === "POST" && url.pathname === "/internal/devices/pairings/consume") {
      return this.consumePairing(request);
    }
    if (request.method === "POST" && url.pathname === "/internal/devices/authenticate") {
      return this.authenticateDevice(request);
    }
    if (request.method === "POST" && url.pathname === "/internal/devices/rename") {
      return this.renameDevice(request);
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

    // Revocation is an irreversible, dedicated operation. An arbitrary PUT must
    // never create a revoked transition, and must never resurrect a revoked
    // device back to active/suspended. This is required for "any revoke" to be
    // irreversible and for reconnect denial to hold.
    if (parsed.authorization === "revoked") {
      return json({ ok: false, code: "revoke_via_put_forbidden" }, 409);
    }
    const existing = parseDeviceRecord(await this.state.storage.get<DeviceRecord>(DEVICE_PREFIX + deviceId));
    if (existing && existing.authorization === "revoked") {
      return json({ ok: false, code: "device_revoked" }, 409);
    }

    await this.state.storage.put(DEVICE_PREFIX + deviceId, parsed);
    return json({ ok: true, device: parsed });
  }

  private async createPairing(request: Request): Promise<Response> {
    let body: unknown;
    try {
      body = await request.json();
    } catch {
      return json({ ok: false, code: "bad_request" }, 400);
    }
    if (!isRecord(body)) return json({ ok: false, code: "bad_request" }, 400);
    const ttlMs = normalizePairingTtlMs(body.ttl_seconds);
    if (ttlMs === null) return json({ ok: false, code: "invalid_pairing_ttl" }, 400);
    const name = normalizeOptionalName(body.name);
    if (body.name !== undefined && name === null) return json({ ok: false, code: "invalid_device_name" }, 400);
    const workerContext = typeof body.worker_context === "string" && body.worker_context.length > 0 && body.worker_context.length <= 256
      ? body.worker_context
      : null;
    if (workerContext === null) return json({ ok: false, code: "bad_request" }, 400);

    const pepper = getPairingPepper(this.env);
    if (pepper === null) return json({ ok: false, code: "pairing_unavailable" }, 503);

    const pairingId = newPairingId();
    const code = newPairingCode();
    const now = Date.now();
    const verifier = await pairingVerifier(pairingId, code, workerContext, pepper);
    const record: PairingRecord = {
      verifier_sha256: verifier,
      created_at_ms: now,
      expires_at_ms: now + ttlMs,
      attempts: 0,
      state: "pending",
      name,
    };

    // At most one active unconsumed pairing session per Worker: creating a new
    // session atomically invalidates (replaces) any prior one, so the owner
    // never has to wait out an abandoned session. The DO transaction is the
    // serialization point; a concurrent consume either sees the old or the new
    // record, never both.
    const stored = await this.state.storage.transaction(async (tx) => {
      const prior = await tx.list({ prefix: PAIRING_PREFIX });
      for (const key of prior.keys()) await tx.delete(key);
      const key = await pairingStorageKey(pairingId);
      await tx.put(key, record);
      return key;
    });
    void stored;
    // Raw pairing_id and code are returned exactly once and never stored or logged.
    return json({ ok: true, pairing_id: pairingId, code, expires_at_ms: record.expires_at_ms });
  }

  private async consumePairing(request: Request): Promise<Response> {
    let body: unknown;
    try {
      body = await request.json();
    } catch {
      return json({ ok: false, code: "bad_request" }, 400);
    }
    if (!isRecord(body)) return json({ ok: false, code: "bad_request" }, 400);
    if (!isPairingId(body.pairing_id) || !isPairingCode(body.code)) {
      return json({ ok: false, code: "invalid_pairing_request" }, 400);
    }
    const requestedName = normalizeOptionalName(body.name);
    if (body.name !== undefined && requestedName === null) return json({ ok: false, code: "invalid_device_name" }, 400);
    const workerContext = typeof body.worker_context === "string" && body.worker_context.length > 0 && body.worker_context.length <= 256
      ? body.worker_context
      : null;
    if (workerContext === null) return json({ ok: false, code: "bad_request" }, 400);

    const pepper = getPairingPepper(this.env);
    if (pepper === null) return json({ ok: false, code: "pairing_rejected" }, 401);

    const storageKey = await pairingStorageKey(body.pairing_id);
    const expectedVerifier = await pairingVerifier(body.pairing_id, body.code, workerContext, pepper);
    const now = Date.now();

    // The DO transaction is the authoritative serialization point: concurrent
    // consumes interleave only here, so exactly one ever sees the live record.
    const result = await this.state.storage.transaction(async (tx) => {
      const record = await tx.get<PairingRecord>(storageKey);
      if (!record) return { ok: false as const, code: "pairing_rejected" as const };
      if (record.state === "locked") return { ok: false as const, code: "pairing_rejected" as const };
      if (record.expires_at_ms <= now) {
        await tx.delete(storageKey);
        return { ok: false as const, code: "pairing_rejected" as const };
      }
      if (!constantTimeEqual(enc.encode(record.verifier_sha256), enc.encode(expectedVerifier))) {
        const attempts = record.attempts + 1;
        const locked = attempts >= MAX_PAIRING_ATTEMPTS;
        await tx.put(storageKey, { ...record, attempts, state: locked ? "locked" : record.state });
        return { ok: false as const, code: "pairing_rejected" as const };
      }

      const deviceId = newDeviceId(now);
      const credentialId = newCredentialId();
      const deviceSecret = newDeviceSecret();
      const device: DeviceRecord = {
        device_id: deviceId,
        workstation_id: deviceId,
        name: record.name ?? requestedName ?? deviceId,
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
        workstation_id: deviceId,
        verifier_sha256: await sha256Hex(deviceSecret),
        created_at_ms: now,
      };
      await tx.delete(storageKey);
      await tx.put(DEVICE_PREFIX + deviceId, device);
      await tx.put(WORKSTATION_PREFIX + deviceId, deviceId);
      await tx.put(CREDENTIAL_PREFIX + credentialId, credential);
      return { ok: true as const, device_id: deviceId, credential_id: credentialId, device_secret: deviceSecret };
    });

    if (!result.ok) return json(result, 401);
    return json({
      ok: true,
      device_id: result.device_id,
      workstation_id: result.device_id,
      credential_id: result.credential_id,
      device_secret: result.device_secret,
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

  private async renameDevice(request: Request): Promise<Response> {
    let body: unknown;
    try {
      body = await request.json();
    } catch {
      return json({ ok: false, code: "bad_request" }, 400);
    }
    if (!isRecord(body) || !isWorkstationId(body.workstation_id)) {
      return json({ ok: false, code: "bad_request" }, 400);
    }
    const name = normalizeOptionalName(body.name);
    if (name === null) return json({ ok: false, code: "invalid_device_name" }, 400);

    const deviceId = await this.state.storage.get<string>(WORKSTATION_PREFIX + body.workstation_id);
    if (!deviceId) return json({ ok: false, code: "device_not_found" }, 404);
    const existing = parseDeviceRecord(await this.state.storage.get<DeviceRecord>(DEVICE_PREFIX + deviceId));
    if (!existing) return json({ ok: false, code: "registry_corrupt" }, 409);
    if (existing.authorization === "revoked") {
      return json({ ok: false, code: "device_revoked" }, 409);
    }
    if (existing.name === name) {
      return json({ ok: true, device_id: existing.device_id, name, updated_at_ms: existing.updated_at_ms, wrote_registry: false });
    }

    const renamed: DeviceRecord = {
      ...existing,
      name,
      aliases: [existing.name, ...(existing.aliases ?? [])]
        .filter((alias, index, aliases) => alias !== name && aliases.indexOf(alias) === index)
        .slice(0, 32),
      updated_at_ms: Date.now(),
    };
    await this.state.storage.put(DEVICE_PREFIX + deviceId, renamed);
    return json({ ok: true, device_id: renamed.device_id, name: renamed.name, updated_at_ms: renamed.updated_at_ms, wrote_registry: true });
  }

  private async revokeDevice(deviceId: string): Promise<Response> {
    const canonical = normalizeDeviceId(deviceId);
    if (!canonical) return json({ ok: false, code: "invalid_device_id" }, 400);
    const existing = parseDeviceRecord(await this.state.storage.get<DeviceRecord>(DEVICE_PREFIX + canonical));
    if (!existing) return json({ ok: false, code: "device_not_found" }, 404);

    const now = Date.now();
    const alreadyRevoked = existing.authorization === "revoked";
    let wroteRegistry = false;
    if (!alreadyRevoked) {
      const revoked: DeviceRecord = {
        ...existing,
        authorization: "revoked",
        revoked_at_ms: existing.revoked_at_ms ?? now,
        updated_at_ms: now,
      };
      await this.state.storage.put(DEVICE_PREFIX + canonical, revoked);
      wroteRegistry = true;
    }

    // Kill switch: tear down the target device's live WorkstationDO/WebSocket
    // session. The workstation_id comes from the stored record — never from the
    // caller — so a revoke can only ever target the device it is bound to.
    const workstationId = existing.workstation_id;
    let teardownOk = false;
    try {
      if (this.env.WORKSTATION_DO) {
        const stub = this.env.WORKSTATION_DO.get(this.env.WORKSTATION_DO.idFromName(workstationId));
        const response = await stub.fetch(new Request("https://do.internal/internal/revoke", { method: "POST" }));
        teardownOk = response.ok;
      }
    } catch {
      teardownOk = false;
    }

    if (!teardownOk) {
      // Authorization is already revoked (fail-closed): reconnect and new
      // routing already deny. A retry performs no registry write and retries
      // teardown, so this converges. Surface a retryable error.
      return json(
        { ok: false, code: "revoke_teardown_failed", retryable: true, device_id: canonical, revoked: true },
        503,
      );
    }
    return json({ ok: true, device_id: canonical, revoked: true, revoked_at_ms: existing.revoked_at_ms ?? now, wrote_registry: wroteRegistry });
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

function normalizePairingTtlMs(value: unknown): number | null {
  if (value === undefined) return DEFAULT_PAIRING_TTL_MS;
  if (typeof value !== "number" || !Number.isSafeInteger(value)) return null;
  const ttlMs = value * 1000;
  return ttlMs >= MIN_PAIRING_TTL_MS && ttlMs <= MAX_PAIRING_TTL_MS ? ttlMs : null;
}

function normalizeOptionalName(value: unknown): string | null {
  if (value === undefined || value === null) return null;
  if (typeof value !== "string") return null;
  const name = value.trim();
  return name.length > 0 && name.length <= 128 ? name : null;
}
