export const DEVICE_ID_PREFIX = "dev_" as const;

const DEVICE_ULID_RE = /^[0-7][0-9A-HJKMNP-TV-Z]{25}$/i;
const WORKSTATION_ID_RE = /^[A-Za-z0-9_.-]{1,64}$/;
const CROCKFORD_BASE32 = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";
const MAX_ULID_TIME_MS = 0xffffffffffff;

export type DeviceAuthorization = "active" | "suspended" | "revoked";
export type DeviceScheduling = "enabled" | "draining" | "paused";

/** Durable identity/desired-state record. Realtime connection state belongs to WorkstationDO. */
export interface DeviceRecord {
  device_id: string;
  workstation_id: string;
  name: string;
  authorization: DeviceAuthorization;
  scheduling: DeviceScheduling;
  credential_id: string | null;
  enrolled_at_ms: number;
  updated_at_ms: number;
  revoked_at_ms: number | null;
}

export function normalizeDeviceId(value: string): string | null {
  const trimmed = value.trim();
  if (!trimmed.startsWith(DEVICE_ID_PREFIX)) return null;
  const suffix = trimmed.slice(DEVICE_ID_PREFIX.length);
  if (!DEVICE_ULID_RE.test(suffix)) return null;
  return `${DEVICE_ID_PREFIX}${suffix.toUpperCase()}`;
}

function encodeBase32(value: bigint, length: number): string {
  let encoded = "";
  for (let i = 0; i < length; i += 1) {
    encoded = CROCKFORD_BASE32[Number(value & 31n)] + encoded;
    value >>= 5n;
  }
  return encoded;
}

export function newDeviceId(nowMs = Date.now(), randomBytes?: Uint8Array): string {
  if (!Number.isSafeInteger(nowMs) || nowMs < 0 || nowMs > MAX_ULID_TIME_MS) {
    throw new Error("device id timestamp is outside the ULID range");
  }
  const bytes = randomBytes ?? crypto.getRandomValues(new Uint8Array(10));
  if (bytes.length !== 10) throw new Error("device id randomness must be exactly 10 bytes");
  let random = 0n;
  for (const byte of bytes) random = (random << 8n) | BigInt(byte);
  return `${DEVICE_ID_PREFIX}${encodeBase32(BigInt(nowMs), 10)}${encodeBase32(random, 16)}`;
}

export function isDeviceId(value: unknown): value is string {
  return typeof value === "string" && normalizeDeviceId(value) !== null;
}

export function isWorkstationId(value: unknown): value is string {
  return typeof value === "string" && WORKSTATION_ID_RE.test(value);
}

export function isRoutableDevice(record: DeviceRecord): boolean {
  return record.authorization === "active" && record.scheduling === "enabled";
}

export function validateDeviceRecord(record: DeviceRecord): string | null {
  const normalizedId = normalizeDeviceId(record.device_id);
  if (normalizedId === null || normalizedId !== record.device_id) return "invalid_device_id";
  if (!isWorkstationId(record.workstation_id)) return "invalid_workstation_id";
  if (record.name.trim().length === 0 || record.name.length > 128) return "invalid_device_name";
  if (!Number.isSafeInteger(record.enrolled_at_ms) || record.enrolled_at_ms < 0) return "invalid_enrolled_at";
  if (!Number.isSafeInteger(record.updated_at_ms) || record.updated_at_ms < record.enrolled_at_ms) {
    return "invalid_updated_at";
  }
  if (record.revoked_at_ms !== null && (!Number.isSafeInteger(record.revoked_at_ms) || record.revoked_at_ms < 0)) {
    return "invalid_revoked_at";
  }
  if (record.authorization === "revoked" && record.revoked_at_ms === null) return "missing_revoked_at";
  if (record.authorization !== "revoked" && record.revoked_at_ms !== null) return "unexpected_revoked_at";
  return null;
}
