/**
 * validation.ts — strict shape/limit validation for Relay Protocol v1.
 *
 * Two-stage defense for untrusted inbound frames:
 *
 *   1. Raw byte gate BEFORE parsing: the encoded UTF-8 frame must fit within
 *      `maxFrameBytes`, otherwise it is rejected as `frame_too_large` without
 *      ever being JSON.parsed (bounded memory, platform-limit friendly).
 *   2. Post-parse structural gate: `parseRelayMessage` shape-checks every
 *      field, rejects unknown fields for the message kind (unless
 *      `strictUnknownFields` is disabled), validates identifiers against
 *      conservative ASCII patterns and length bounds, and enforces nested
 *      bounds (nesting depth, object key count, array item count, string
 *      length, byte budgets for arguments/result/details/trace).
 *
 * Results are values (`RelayCheck` / parse results), never throws, so callers
 * can route a rejection to a structured wire "error"/tool_error frame.
 *
 * Identifier grammar (documented):
 *   - workstation_id: 1..64 chars, `[A-Za-z0-9][A-Za-z0-9._:-]*`
 *   - request_id / boot_id / idempotency_key: 1..128 chars, same grammar
 *     (UUIDs and opaque ids like "req_01H…", "boot-2026-…" are valid)
 *   - connection/link tokens that must ride headers are NOT identifiers and
 *     are out of scope here (edge scope owns handshake auth).
 */

import {
  CORRELATED_KINDS,
  MESSAGE_KINDS,
  RELAY_PROTOCOL_VERSION,
  type RelayCheck,
  type RelayMessage,
  type RelayValidationCode,
} from "./protocol.js";
import { canonicalJson } from "./canonical-json.js";

// ─────────────────────────────────────────────────────────────────────────────
// Limits (shared defaults; override via RelayValidationOptions)
// ─────────────────────────────────────────────────────────────────────────────

export const MAX_WORKSTATION_ID_LEN = 64;
export const MAX_REQUEST_ID_LEN = 128;
export const MAX_BOOT_ID_LEN = 128;
export const MAX_IDEMPOTENCY_KEY_LEN = 128;
export const MAX_OPERATION_LEN = 256;
export const MAX_LINK_VERSION_LEN = 128;
export const MAX_STRING_LEN = 4096;
export const MAX_SHA256_HASH_LEN = 71; // "sha256:" + 64 hex chars
export const MAX_CAPABILITIES = 32;
export const MAX_CAPABILITY_LEN = 128;
export const MAX_ARGS_JSON_BYTES = 256 * 1024; // 256 KiB
export const MAX_RESULT_JSON_BYTES = 1 * 1024 * 1024; // 1 MiB
export const MAX_DETAILS_JSON_BYTES = 64 * 1024; // 64 KiB
export const MAX_TRACE_JSON_BYTES = 16 * 1024; // 16 KiB
export const MAX_NESTING_DEPTH = 32;
export const MAX_KEYS_PER_OBJECT = 512;
export const MAX_ITEMS_PER_ARRAY = 4096;
/** Default raw frame byte budget (well below the DO 32 MiB platform cap). */
export const DEFAULT_MAX_FRAME_BYTES = 1024 * 1024; // 1 MiB
/** Defaults for timeout_ms on the wire: 1s..60s (mirrors local RPC cap). */
export const MIN_TIMEOUT_MS = 1_000;
export const MAX_TIMEOUT_MS = 60_000;

export interface RelayValidationOptions {
  /** Raw UTF-8 frame byte budget, pre-parse. */
  maxFrameBytes?: number;
  /** Byte budget for tool_request `arguments` (serialized). */
  maxArgsBytes?: number;
  /** Byte budget for tool_result `result` (serialized). */
  maxResultBytes?: number;
  /** Byte budget for tool_error `details` (serialized). */
  maxDetailsBytes?: number;
  /** Byte budget for tool_request `trace` (serialized). */
  maxTraceBytes?: number;
  /** Reject fields not declared for a message kind (default true). */
  strictUnknownFields?: boolean;
}

/** Normalize options with defaults. */
export function normalizeOptions(raw?: RelayValidationOptions): Required<RelayValidationOptions> {
  return {
    maxFrameBytes: raw?.maxFrameBytes ?? DEFAULT_MAX_FRAME_BYTES,
    maxArgsBytes: raw?.maxArgsBytes ?? MAX_ARGS_JSON_BYTES,
    maxResultBytes: raw?.maxResultBytes ?? MAX_RESULT_JSON_BYTES,
    maxDetailsBytes: raw?.maxDetailsBytes ?? MAX_DETAILS_JSON_BYTES,
    maxTraceBytes: raw?.maxTraceBytes ?? MAX_TRACE_JSON_BYTES,
    strictUnknownFields: raw?.strictUnknownFields ?? true,
  };
}

export function utf8ByteLength(text: string): number {
  return new TextEncoder().encode(text).length;
}

// ─────────────────────────────────────────────────────────────────────────────
// Raw frame gate
// ─────────────────────────────────────────────────────────────────────────────

/**
 * Reject oversized raw frames BEFORE parsing. `maxBytes` is the caller's
 * configured budget (defaults to `normalizeOptions` default inside
 * `parseRelayFrame`).
 */
export function checkFrameBytes(raw: string, maxBytes: number): RelayCheck {
  const bytes = utf8ByteLength(raw);
  if (bytes > maxBytes) {
    return { ok: false, code: "frame_too_large", reason: `frame is ${bytes} bytes; budget is ${maxBytes}` };
  }
  return { ok: true };
}

// ─────────────────────────────────────────────────────────────────────────────
// Identifier validation
// ─────────────────────────────────────────────────────────────────────────────

const ID_GRAMMAR = /^[A-Za-z0-9][A-Za-z0-9._:-]*$/;

export function isValidIdentifier(value: unknown, minLen: number, maxLen: number): value is string {
  if (typeof value !== "string") return false;
  if (value.length < minLen || value.length > maxLen) return false;
  return ID_GRAMMAR.test(value);
}

function checkIdentifier(
  value: unknown,
  name: string,
  minLen: number,
  maxLen: number,
  code: RelayValidationCode,
): RelayCheck {
  if (!isValidIdentifier(value, minLen, maxLen)) {
    return { ok: false, code, reason: `${name} must match ${ID_GRAMMAR} within ${minLen}..${maxLen} chars` };
  }
  return { ok: true };
}

export function checkWorkstationId(value: unknown): RelayCheck {
  return checkIdentifier(value, "workstation_id", 1, MAX_WORKSTATION_ID_LEN, "invalid_workstation_id");
}

export function checkRequestId(value: unknown): RelayCheck {
  return checkIdentifier(value, "request_id", 1, MAX_REQUEST_ID_LEN, "invalid_request_id");
}

export function checkBootId(value: unknown): RelayCheck {
  return checkIdentifier(value, "boot_id", 1, MAX_BOOT_ID_LEN, "invalid_boot_id");
}

// ─────────────────────────────────────────────────────────────────────────────
// Value-level bounds
// ─────────────────────────────────────────────────────────────────────────────

function fail(code: RelayValidationCode, reason: string): RelayCheck {
  return { ok: false, code, reason };
}

function checkString(value: unknown, name: string, maxLen = MAX_STRING_LEN): RelayCheck {
  if (typeof value !== "string") return fail("invalid_string", `${name} must be a string`);
  if (value.length === 0) return fail("invalid_string", `${name} must not be empty`);
  if (value.length > maxLen) return fail("string_too_long", `${name} exceeds ${maxLen} chars`);
  return { ok: true };
}

function checkStringOrNull(value: unknown, name: string, maxLen = MAX_STRING_LEN): RelayCheck {
  if (value === null) return { ok: true };
  if (value === undefined) return fail("invalid_string", `${name} must be null or a string`);
  return checkString(value, name, maxLen);
}

/** Optional field: absent (undefined) is fine; otherwise null or bounded string. */
function checkOptionalStringOrNull(value: unknown, name: string, maxLen = MAX_STRING_LEN): RelayCheck {
  if (value === undefined) return { ok: true };
  return checkStringOrNull(value, name, maxLen);
}

function checkNumber(value: unknown, name: string, min: number, max: number): RelayCheck {
  if (typeof value !== "number" || !Number.isFinite(value)) {
    return fail("invalid_number", `${name} must be a finite number`);
  }
  if (value < min || value > max) return fail("invalid_number", `${name} must be within ${min}..${max}`);
  return { ok: true };
}

/** Structural bounds on any value tree (depth/keys/items/string length). */
export function checkTreeBounds(
  value: unknown,
  opts: { maxDepth?: number; maxKeys?: number; maxItems?: number; maxStringLen?: number } = {},
): RelayCheck {
  const maxDepth = opts.maxDepth ?? MAX_NESTING_DEPTH;
  const maxKeys = opts.maxKeys ?? MAX_KEYS_PER_OBJECT;
  const maxItems = opts.maxItems ?? MAX_ITEMS_PER_ARRAY;
  const maxStringLen = opts.maxStringLen ?? MAX_STRING_LEN;
  let code: RelayValidationCode | null = null;
  let reason = "";
  const walk = (v: unknown, depth: number): void => {
    if (code) return;
    if (depth > maxDepth) {
      code = "too_deep";
      reason = `nesting exceeds ${maxDepth} levels`;
      return;
    }
    if (v === null || typeof v === "boolean" || typeof v === "number") return;
    if (typeof v === "string") {
      if (v.length > maxStringLen) {
        code = "string_too_long";
        reason = `string exceeds ${maxStringLen} chars`;
      }
      return;
    }
    if (typeof v !== "object") {
      code = "invalid_result";
      reason = "unsupported value type in payload tree";
      return;
    }
    if (Array.isArray(v)) {
      if (v.length > maxItems) {
        code = "too_many_items";
        reason = `array exceeds ${maxItems} items`;
        return;
      }
      for (const item of v) walk(item, depth + 1);
      return;
    }
    const obj = v as Record<string, unknown>;
    const keys = Object.keys(obj);
    if (keys.length > maxKeys) {
      code = "too_many_keys";
      reason = `object exceeds ${maxKeys} keys`;
      return;
    }
    for (const k of keys) {
      if (k.length > maxStringLen) {
        code = "string_too_long";
        reason = `object key exceeds ${maxStringLen} chars`;
        return;
      }
      walk(obj[k], depth + 1);
    }
  };
  walk(value, 0);
  return code ? fail(code, reason) : { ok: true };
}

/**
 * Budget an arbitrary payload (arguments/result/details/trace) both by
 * serialized byte size and by structural bounds. Structural bounds are
 * checked FIRST so deep/tall payloads are rejected as too_deep and not
 * misattributed by a canonicalizer depth throw; canonicalJson is used for
 * the byte accounting so it has no dependency on key insertion order.
 */
export function checkPayloadBudget(value: unknown, maxBytes: number): RelayCheck {
  // String length is governed by the byte budget, not the 4096-char metadata
  // cap: tool arguments/results may legitimately contain large strings (file
  // contents, transcripts). The byte-budget check below is the real string
  // bound, so structural string length is only sanity-capped well above the
  // budget to avoid double-reporting; depth/keys/items still enforce
  // structure.
  const bounds = checkTreeBounds(value, { maxStringLen: Math.max(MAX_STRING_LEN, maxBytes * 4) });
  if (!bounds.ok) return bounds;
  let size: number;
  try {
    size = utf8ByteLength(canonicalJson(value, MAX_NESTING_DEPTH));
  } catch (err) {
    return fail("invalid_arguments", `payload is not canonical JSON: ${err instanceof Error ? err.message : String(err)}`);
  }
  if (size > maxBytes) return fail("payload_too_large", `payload is ${size} bytes; budget is ${maxBytes}`);
  return { ok: true };
}

// ─────────────────────────────────────────────────────────────────────────────
// Per-kind declared fields (strict unknown-field gate)
// ─────────────────────────────────────────────────────────────────────────────

export const KIND_FIELDS: Record<RelayMessage["kind"], ReadonlySet<string>> = {
  hello: new Set(["protocol_version", "kind", "workstation_id", "boot_id", "link_version", "connected_at_ms", "capabilities", "runtime"]),
  hello_ack: new Set([
    "protocol_version",
    "kind",
    "workstation_id",
    "ok",
    "server_version",
    "edge_deployment_id",
    "capabilities",
    "reconnect",
    "resume",
    "completed",
    "code",
    "message",
  ]),
  heartbeat: new Set(["protocol_version", "kind", "workstation_id", "boot_id", "sent_at_ms", "link_uptime_ms", "active_requests", "runtime"]),
  status: new Set([
    "protocol_version",
    "kind",
    "workstation_id",
    "query",
    "boot_id",
    "runtime",
    "runtime_generation",
    "healthy",
    "health_details",
    "active_requests",
    "link_uptime_ms",
    "last_error",
    "sent_at_ms",
  ]),
  tool_request: new Set([
    "protocol_version",
    "kind",
    "workstation_id",
    "request_id",
    "operation",
    "arguments",
    "timeout_ms",
    "contract_epoch",
    "contract_hash",
    "idempotency_key",
    "trace",
  ]),
  tool_result: new Set([
    "protocol_version",
    "kind",
    "workstation_id",
    "request_id",
    "result",
    "served_at_ms",
    "runtime_generation",
    "transport_name",
  ]),
  tool_error: new Set([
    "protocol_version",
    "kind",
    "workstation_id",
    "request_id",
    "code",
    "message",
    "details",
    "retryable",
    "delivery_state",
    "served_at_ms",
    "runtime_generation",
  ]),
  cancel: new Set(["protocol_version", "kind", "workstation_id", "request_id", "reason"]),
  cancel_ack: new Set(["protocol_version", "kind", "workstation_id", "request_id", "accepted", "cancelled_at_ms", "reason"]),
};

// ─────────────────────────────────────────────────────────────────────────────
// Message validation
// ─────────────────────────────────────────────────────────────────────────────

function checkRuntimeContract(value: unknown): RelayCheck {
  if (value === undefined) return { ok: true };
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    return fail("invalid_runtime", "runtime must be an object");
  }
  const r = value as Record<string, unknown>;
  const c = checkString(r.runtime_version, "runtime.runtime_version");
  if (!c.ok) return c;
  const commit = checkStringOrNull(r.runtime_commit, "runtime.runtime_commit");
  if (!commit.ok) return commit;
  const gen = checkStringOrNull(r.runtime_generation, "runtime.runtime_generation");
  if (!gen.ok) return gen;
  const epoch = checkNumber(r.contract_epoch, "runtime.contract_epoch", 0, 1_000_000);
  if (!epoch.ok) return epoch;
  const hash = checkStringOrNull(r.contract_hash, "runtime.contract_hash", MAX_SHA256_HASH_LEN);
  if (!hash.ok) return hash;
  const hv = checkStringOrNull(r.herdr_version, "runtime.herdr_version");
  if (!hv.ok) return hv;
  const hp = checkStringOrNull(r.herdr_protocol, "runtime.herdr_protocol");
  if (!hp.ok) return hp;
  return { ok: true };
}

function checkCapabilities(value: unknown): RelayCheck {
  if (value === undefined) return { ok: true };
  if (!Array.isArray(value)) return fail("invalid_capabilities", "capabilities must be an array");
  if (value.length > MAX_CAPABILITIES) return fail("too_many_items", `capabilities exceed ${MAX_CAPABILITIES}`);
  for (const cap of value) {
    if (typeof cap !== "string") return fail("invalid_capabilities", "capabilities must be strings");
    if (cap.length === 0 || cap.length > MAX_CAPABILITY_LEN) {
      return fail("invalid_capabilities", `capability exceeds ${MAX_CAPABILITY_LEN} chars`);
    }
  }
  return { ok: true };
}

function checkResumeSummary(value: unknown): RelayCheck {
  if (!Array.isArray(value)) return fail("invalid_result", "resume must be an array");
  if (value.length > 4096) return fail("too_many_items", "resume exceeds 4096 entries");
  for (const item of value) {
    if (item === null || typeof item !== "object") return fail("invalid_result", "resume entries must be objects");
    const rec = item as Record<string, unknown>;
    const rid = checkRequestId(rec.request_id);
    if (!rid.ok) return rid;
    const op = checkString(rec.operation, "resume.operation", MAX_OPERATION_LEN);
    if (!op.ok) return op;
    const state = checkString(rec.state, "resume.state");
    if (!state.ok) return state;
    if (rec.state !== "queued" && rec.state !== "sent" && rec.state !== "settled") {
      return fail("invalid_enum", "resume.state must be queued|sent|settled");
    }
    const dl = checkNumber(rec.deadline_ms, "resume.deadline_ms", 0, Number.MAX_SAFE_INTEGER);
    if (!dl.ok) return dl;
  }
  return { ok: true };
}

/**
 * Strict shape/limit validation of one parsed message object. Returns the
 * message when valid, or a stable rejection. `request_id` checks are applied
 * per CORRELATED_KINDS, unknown fields are rejected per KIND_FIELDS when
 * `strictUnknownFields` is enabled.
 */
export function validateRelayMessage(value: unknown, rawOpts?: RelayValidationOptions): RelayCheck {
  const opts = normalizeOptions(rawOpts);
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    return fail("not_object", "message must be a JSON object");
  }
  const obj = value as Record<string, unknown>;
  if (obj.protocol_version === undefined) return fail("missing_protocol_version", "protocol_version is required");
  if (obj.protocol_version !== RELAY_PROTOCOL_VERSION) {
    return fail(
      "unsupported_protocol_version",
      `unsupported protocol_version ${String(obj.protocol_version)} (relay v1 expects ${RELAY_PROTOCOL_VERSION})`,
    );
  }
  if (obj.kind === undefined) return fail("missing_kind", "kind is required");
  if (typeof obj.kind !== "string" || !(MESSAGE_KINDS as readonly string[]).includes(obj.kind)) {
    return fail("unknown_kind", `unknown message kind ${String(obj.kind)}`);
  }
  const kind = obj.kind as RelayMessage["kind"];
  if (obj.workstation_id === undefined) return fail("missing_workstation_id", "workstation_id is required");
  const ws = checkWorkstationId(obj.workstation_id);
  if (!ws.ok) return ws;

  const isCorrelated = CORRELATED_KINDS.has(kind);
  const hasRequestId = obj.request_id !== undefined;
  if (isCorrelated && !hasRequestId) return fail("missing_request_id", `${kind} requires request_id`);
  if (!isCorrelated && hasRequestId) {
    return fail("unexpected_request_id", `${kind} must not carry request_id (control message)`);
  }
  if (hasRequestId) {
    const rid = checkRequestId(obj.request_id);
    if (!rid.ok) return rid;
  }

  if (opts.strictUnknownFields) {
    const allowed = KIND_FIELDS[kind];
    for (const key of Object.keys(obj)) {
      if (!allowed.has(key)) return fail("unknown_field", `unknown field '${key}' for kind '${kind}'`);
    }
  }

  switch (kind) {
    case "hello": {
      const boot = checkBootId(obj.boot_id);
      if (!boot.ok) return boot;
      const lv = checkString(obj.link_version, "link_version", MAX_LINK_VERSION_LEN);
      if (!lv.ok) return lv;
      const caps = checkCapabilities(obj.capabilities);
      if (!caps.ok) return caps;
      return checkRuntimeContract(obj.runtime);
    }
    case "hello_ack": {
      if (typeof obj.ok !== "boolean") return fail("invalid_boolean", "hello_ack.ok must be a boolean");
      if (!obj.ok) {
        const code = checkString(obj.code, "hello_ack.code", 128);
        if (!code.ok) return code;
        const msg = checkString(obj.message, "hello_ack.message");
        if (!msg.ok) return msg;
        return { ok: true };
      }
      for (const f of ["server_version", "edge_deployment_id"] as const) {
        if (obj[f] !== undefined) {
          const c = checkString(obj[f], `hello_ack.${f}`, 128);
          if (!c.ok) return c;
        }
      }
      const caps = checkCapabilities(obj.capabilities);
      if (!caps.ok) return caps;
      if (obj.reconnect !== undefined && typeof obj.reconnect !== "boolean") {
        return fail("invalid_boolean", "hello_ack.reconnect must be a boolean");
      }
      if (obj.resume !== undefined) {
        const resume = checkResumeSummary(obj.resume);
        if (!resume.ok) return resume;
      }
      if (obj.completed !== undefined) {
        if (!Array.isArray(obj.completed) || obj.completed.length > 512) {
          return fail("too_many_items", "completed exceeds 512 entries or is not an array");
        }
        for (const id of obj.completed) {
          const c = checkRequestId(id);
          if (!c.ok) return c;
        }
      }
      return { ok: true };
    }
    case "heartbeat": {
      const boot = checkBootId(obj.boot_id);
      if (!boot.ok) return boot;
      const sent = checkNumber(obj.sent_at_ms, "sent_at_ms", 0, Number.MAX_SAFE_INTEGER);
      if (!sent.ok) return sent;
      const act = checkNumber(obj.active_requests, "active_requests", 0, 1_000_000);
      if (!act.ok) return act;
      return checkRuntimeContract(obj.runtime);
    }
    case "status": {
      if (obj.query !== undefined && typeof obj.query !== "boolean") {
        return fail("invalid_boolean", "status.query must be a boolean");
      }
      const rt = checkRuntimeContract(obj.runtime);
      if (!rt.ok) return rt;
      for (const f of ["runtime_generation", "health_details", "last_error"] as const) {
        const c = checkOptionalStringOrNull(obj[f], `status.${f}`);
        if (!c.ok) return c;
      }
      if (obj.healthy !== undefined && typeof obj.healthy !== "boolean") {
        return fail("invalid_boolean", "status.healthy must be a boolean");
      }
      if (obj.active_requests !== undefined) {
        const c = checkNumber(obj.active_requests, "active_requests", 0, 1_000_000);
        if (!c.ok) return c;
      }
      if (obj.link_uptime_ms !== undefined || obj.sent_at_ms !== undefined) {
        for (const f of ["link_uptime_ms", "sent_at_ms"] as const) {
          if (obj[f] === undefined) continue;
          const c = checkNumber(obj[f], `status.${f}`, 0, Number.MAX_SAFE_INTEGER);
          if (!c.ok) return c;
        }
      }
      if (obj.boot_id !== undefined) {
        const c = checkBootId(obj.boot_id);
        if (!c.ok) return c;
      }
      return { ok: true };
    }
    case "tool_request": {
      const op = checkString(obj.operation, "operation", MAX_OPERATION_LEN);
      if (!op.ok) return op;
      if (obj.timeout_ms !== undefined) {
        const c = checkNumber(obj.timeout_ms, "timeout_ms", MIN_TIMEOUT_MS, MAX_TIMEOUT_MS);
        if (!c.ok) return c;
      }
      if (obj.contract_epoch !== undefined) {
        const c = checkNumber(obj.contract_epoch, "contract_epoch", 0, 1_000_000);
        if (!c.ok) return c;
      }
      if (obj.contract_hash !== undefined) {
        const c = checkString(obj.contract_hash, "contract_hash", MAX_SHA256_HASH_LEN);
        if (!c.ok) return c;
      }
      if (obj.idempotency_key !== undefined) {
        if (!isValidIdentifier(obj.idempotency_key, 1, MAX_IDEMPOTENCY_KEY_LEN)) {
          return fail("invalid_id", "idempotency_key must match identifier grammar within 1..128 chars");
        }
      }
      if (obj.arguments !== undefined) {
        if (obj.arguments === null || typeof obj.arguments !== "object" || Array.isArray(obj.arguments)) {
          return fail("invalid_arguments", "arguments must be an object");
        }
        const c = checkPayloadBudget(obj.arguments, opts.maxArgsBytes);
        if (!c.ok) return c;
      }
      if (obj.trace !== undefined) {
        if (obj.trace === null || typeof obj.trace !== "object" || Array.isArray(obj.trace)) {
          return fail("invalid_result", "trace must be an object");
        }
        const c = checkPayloadBudget(obj.trace, opts.maxTraceBytes);
        if (!c.ok) return c;
      }
      return { ok: true };
    }
    case "tool_result": {
      if (obj.result !== undefined) {
        const c = checkPayloadBudget(obj.result, opts.maxResultBytes);
        if (!c.ok) return c;
      }
      const served = checkNumber(obj.served_at_ms, "served_at_ms", 0, Number.MAX_SAFE_INTEGER);
      if (!served.ok) return served;
      const gen = checkOptionalStringOrNull(obj.runtime_generation, "runtime_generation");
      if (!gen.ok) return gen;
      const tn = checkOptionalStringOrNull(obj.transport_name, "transport_name", 128);
      if (!tn.ok) return tn;
      return { ok: true };
    }
    case "tool_error": {
      const code = checkString(obj.code, "code", 128);
      if (!code.ok) return code;
      if (obj.message !== undefined) {
        const c = checkString(obj.message, "message");
        if (!c.ok) return c;
      }
      if (obj.details !== undefined) {
        const c = checkPayloadBudget(obj.details, opts.maxDetailsBytes);
        if (!c.ok) return c;
      }
      if (typeof obj.retryable !== "boolean") return fail("invalid_boolean", "tool_error.retryable must be a boolean");
      if (obj.delivery_state !== undefined) {
        const c = checkString(obj.delivery_state, "delivery_state", 32);
        if (!c.ok) return c;
        if (obj.delivery_state !== "not_delivered" && obj.delivery_state !== "delivery_unknown" && obj.delivery_state !== "delivered") {
          return fail("invalid_enum", "delivery_state must be not_delivered|delivery_unknown|delivered");
        }
      }
      if (obj.served_at_ms !== undefined) {
        const c = checkNumber(obj.served_at_ms, "served_at_ms", 0, Number.MAX_SAFE_INTEGER);
        if (!c.ok) return c;
      }
      const gen = checkOptionalStringOrNull(obj.runtime_generation, "runtime_generation");
      if (!gen.ok) return gen;
      return { ok: true };
    }
    case "cancel": {
      if (obj.reason !== undefined) {
        const c = checkString(obj.reason, "reason");
        if (!c.ok) return c;
      }
      return { ok: true };
    }
    case "cancel_ack": {
      if (typeof obj.accepted !== "boolean") return fail("invalid_boolean", "cancel_ack.accepted must be a boolean");
      const at = checkNumber(obj.cancelled_at_ms, "cancelled_at_ms", 0, Number.MAX_SAFE_INTEGER);
      if (!at.ok) return at;
      const reason = checkOptionalStringOrNull(obj.reason, "reason");
      if (!reason.ok) return reason;
      return { ok: true };
    }
  }
}

// ─────────────────────────────────────────────────────────────────────────────
// Frame parse path (byte gate → parse → validate)
// ─────────────────────────────────────────────────────────────────────────────

export type ParseResult =
  | { ok: true; message: RelayMessage }
  | { ok: false; code: RelayValidationCode; reason: string };

/**
 * The one entry point for untrusted inbound text: byte gate, JSON parse,
 * strict validation. Returns a structured result instead of throwing so the
 * caller can answer with a wire-level error frame.
 */
export function parseRelayFrame(raw: string, rawOpts?: RelayValidationOptions): ParseResult {
  const opts = normalizeOptions(rawOpts);
  const gate = checkFrameBytes(raw, opts.maxFrameBytes);
  if (!gate.ok) return { ok: false, code: gate.code, reason: gate.reason };
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return { ok: false, code: "not_json", reason: "frame is not valid JSON" };
  }
  const v = validateRelayMessage(parsed, rawOpts);
  if (!v.ok) return { ok: false, code: v.code, reason: v.reason };
  return { ok: true, message: parsed as RelayMessage };
}