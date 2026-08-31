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

export function newEnrollmentCode(): string {
  return `enroll_${randomHex(32)}`;
}

export function newDeviceSecret(): string {
  return `devsec_${randomHex(32)}`;
}

export function newCredentialId(): string {
  return `cred_${randomHex(16)}`;
}

export function isEnrollmentCode(value: unknown): value is string {
  return typeof value === "string" && /^enroll_[0-9a-f]{64}$/.test(value);
}

export function isCredentialVerifier(value: unknown): value is string {
  return typeof value === "string" && /^[0-9a-f]{64}$/.test(value);
}
