const encoder = new TextEncoder();

function bytesToHex(bytes: Uint8Array): string {
  let out = "";
  for (const byte of bytes) out += byte.toString(16).padStart(2, "0");
  return out;
}

export function randomHex(byteLength: number): string {
  const bytes = new Uint8Array(byteLength);
  crypto.getRandomValues(bytes);
  return bytesToHex(bytes);
}

export async function sha256Hex(value: string): Promise<string> {
  const digest = await crypto.subtle.digest("SHA-256", encoder.encode(value));
  return bytesToHex(new Uint8Array(digest));
}

export function newPairingId(): string {
  // 256 bits of CSPRNG entropy; only its SHA-256 digest is ever stored.
  return `pair_${randomHex(32)}`;
}

/**
 * Exactly six decimal digits drawn uniformly from 000000..999999 via CSPRNG
 * rejection sampling, so leading zeros are allowed and every code is
 * equiprobable (no modulo bias).
 */
export function newPairingCode(): string {
  const LIMIT = 1_000_000;
  const CEILING = Math.floor(0x1_0000_0000 / LIMIT) * LIMIT;
  const bytes = new Uint8Array(4);
  for (;;) {
    crypto.getRandomValues(bytes);
    const value =
      (((bytes[0] as number) << 24) | ((bytes[1] as number) << 16) | ((bytes[2] as number) << 8) | (bytes[3] as number)) >>> 0;
    if (value < CEILING) return String(value % LIMIT).padStart(6, "0");
  }
}

export function newDeviceSecret(): string {
  return `devsec_${randomHex(32)}`;
}

export function newCredentialId(): string {
  return `cred_${randomHex(16)}`;
}

export function isPairingId(value: unknown): value is string {
  return typeof value === "string" && /^pair_[0-9a-f]{64}$/.test(value);
}

export function isPairingCode(value: unknown): value is string {
  return typeof value === "string" && /^[0-9]{6}$/.test(value);
}

export function isCredentialVerifier(value: unknown): value is string {
  return typeof value === "string" && /^[0-9a-f]{64}$/.test(value);
}
