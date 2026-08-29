/**
 * payload.ts — byte-budget checks for frames and request bodies.
 *
 * The local runtime is responsible for pagination (plan §11); the edge only
 * enforces that anything it forwards or accepts stays well below platform
 * limits. Pure module: unit-testable in Node without a Workers runtime.
 */

import type { RelayErrorCode } from "./errors.js";

const te = new TextEncoder();

export function byteLengthOf(text: string): number {
  return te.encode(text).length;
}

export type FrameCheck =
  | { ok: true }
  | { ok: false; code: RelayErrorCode; bytes: number; maxBytes: number };

/** Reject oversized raw frames before any parsing/logging. */
export function checkFrameSize(raw: string, maxBytes: number): FrameCheck {
  const bytes = byteLengthOf(raw);
  if (bytes > maxBytes) {
    return { ok: false, code: "payload_too_large", bytes, maxBytes };
  }
  return { ok: true };
}

export type JsonParseResult =
  | { ok: true; value: unknown }
  | { ok: false; code: RelayErrorCode; reason: string };

/** Parse a bounded JSON frame; reject parse failures cleanly. */
export function parseJsonFrame(raw: string, maxBytes: number): JsonParseResult {
  const size = checkFrameSize(raw, maxBytes);
  if (!size.ok) return { ok: false, code: size.code, reason: `frame exceeds ${size.maxBytes} byte budget` };
  try {
    const value = JSON.parse(raw) as unknown;
    return { ok: true, value };
  } catch {
    return { ok: false, code: "bad_request", reason: "frame is not valid JSON" };
  }
}

export interface BodySource {
  headers: { get(name: string): string | null };
  text(): Promise<string>;
}

export interface BytesBodySource {
  headers: { get(name: string): string | null };
  body?: ReadableStream<Uint8Array> | null;
  arrayBuffer(): Promise<ArrayBuffer>;
}

export type BoundedBytesResult =
  | { ok: true; bytes: Uint8Array }
  | { ok: false; code: RelayErrorCode; reason: string };

/**
 * Read a raw request body with an early content-length gate and a hard byte
 * ceiling. Used for binary artifact uploads; JSON MCP bodies stay on
 * `readBodyBounded`.
 */
export async function readBytesBounded(
  body: BytesBodySource,
  maxBytes: number,
): Promise<BoundedBytesResult> {
  const declared = body.headers.get("content-length");
  if (declared !== null) {
    const n = Number.parseInt(declared, 10);
    if (Number.isFinite(n) && n > maxBytes) {
      return { ok: false, code: "payload_too_large", reason: `content-length ${n} exceeds ${maxBytes} budget` };
    }
  }
  if (body.body) {
    const reader = body.body.getReader();
    const chunks: Uint8Array[] = [];
    let total = 0;
    try {
      for (;;) {
        const { done, value } = await reader.read();
        if (done) break;
        if (!value) continue;
        total += value.byteLength;
        if (total > maxBytes) {
          await reader.cancel();
          return { ok: false, code: "payload_too_large", reason: `body exceeds ${maxBytes} budget` };
        }
        chunks.push(value);
      }
    } catch {
      return { ok: false, code: "bad_request", reason: "could not read request body" };
    }
    const bytes = new Uint8Array(total);
    let offset = 0;
    for (const chunk of chunks) {
      bytes.set(chunk, offset);
      offset += chunk.byteLength;
    }
    return { ok: true, bytes };
  }
  try {
    const bytes = new Uint8Array(await body.arrayBuffer());
    if (bytes.byteLength > maxBytes) {
      return { ok: false, code: "payload_too_large", reason: `body exceeds ${maxBytes} budget` };
    }
    return { ok: true, bytes };
  } catch {
    return { ok: false, code: "bad_request", reason: "could not read request body" };
  }
}

/**
 * Read a request body with an early content-length gate plus a post-read byte
 * check. Keeps oversized bodies out of memory as early as possible.
 */
export async function readBodyBounded(
  body: BodySource,
  maxBytes: number,
): Promise<JsonParseResult> {
  const declared = body.headers.get("content-length");
  if (declared !== null) {
    const n = Number.parseInt(declared, 10);
    if (Number.isFinite(n) && n > maxBytes) {
      return { ok: false, code: "payload_too_large", reason: `content-length ${n} exceeds ${maxBytes} budget` };
    }
  }
  let text: string;
  try {
    text = await body.text();
  } catch {
    return { ok: false, code: "bad_request", reason: "could not read request body" };
  }
  return parseJsonFrame(text, maxBytes);
}

/** Budget check for tool arguments before they enter the relay envelope. */
export function checkArgsBudget(args: unknown, maxBytes: number): FrameCheck {
  let json: string;
  try {
    json = JSON.stringify(args ?? null);
  } catch {
    return { ok: false, code: "bad_request", bytes: -1, maxBytes };
  }
  return checkFrameSize(json, maxBytes);
}

/** Budget check for an already-encoded relay wire frame (outbound or inbound). */
export function checkRelayFrameBudget(encoded: string, maxBytes: number): FrameCheck {
  return checkFrameSize(encoded, maxBytes);
}