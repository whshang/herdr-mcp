/**
 * relay-adapter.ts — the SOLE Relay Protocol v1 wire boundary for this edge.
 *
 * OWNERSHIP RULE: Keep every Relay Protocol v1 concept (envelope field names,
 * version strings, encode/decode, protocol-version checks and internal<->wire
 * mapping) inside this file. A separate worker owns `src/relay/**`; that tree
 * MUST NOT be touched. When the workstation side lands, the two sides only
 * need to agree on the envelope shapes and versions exported here.
 *
 * This file duplicates the canonical types from src/relay/protocol.ts because
 * the Worker build is isolated from the project root. Encode/decode/validation
 * MUST be wire-identical: protocol_version = number 1, snake_case field names,
 * canonical kind set.
 *
 * Canonical kinds (from src/relay/protocol.ts):
 *   hello, hello_ack, heartbeat, status, tool_request, tool_result,
 *   tool_error, cancel, cancel_ack
 *
 * Edge → link sends: hello_ack, tool_request, cancel, status (query=true)
 * Link → edge sends: hello, heartbeat, status, tool_result, tool_error, cancel_ack
 *
 * Every frame carries protocol_version, kind, workstation_id.
 * CANNOT be on hello/heartbeat/status; REQUEST_ID is required on
 * tool_request/tool_result/tool_error/cancel/cancel_ack only.
 *
 * Pre-hello: only "hello" is accepted. Any other frame before hello is a WS
 * close (1008/close_rejected). No wire "error" kind is invented for this.
 * After hello the link may send heartbeat, status, tool_result, tool_error,
 * cancel_ack.
 *
 * Draining/upgrade state lives as local DO state only — no wire kind for them.
 */

import {
  RELAY_PROTOCOL_VERSION,
  MAX_WORKSTATION_ID_LEN,
  MAX_BOOT_ID_LEN,
  MAX_REQUEST_ID_LEN,
  MAX_OPERATION_LEN,
  MAX_LINK_VERSION_LEN,
  MAX_CAPABILITIES,
  MAX_CAPABILITY_LEN,
  MAX_SHA256_HASH_LEN,
  MAX_IDEMPOTENCY_KEY_LEN,
  MIN_TIMEOUT_MS,
  MAX_TIMEOUT_MS,
  DEFAULT_MAX_FRAME_BYTES,
  ID_GRAMMAR,
  MESSAGE_KINDS,
  CORRELATED_KINDS,
  isValidIdentifier,
  checkWorkstationId,
  checkRequestId,
  checkBootId,
  validateRelayMessage,
  parseRelayFrame,
  checkPayloadBudget,
  checkFrameBytes,
  utf8ByteLength,
} from "./canonical-imports.js";
import type {
  HelloMessage,
  HelloAckMessage,
  HeartbeatMessage,
  StatusMessage,
  ToolRequestMessage,
  ToolResultMessage,
  ToolErrorMessage,
  CancelMessage,
  CancelAckMessage,
  RelayMessage,
  RelayCheck,
  RelayValidationOptions,
  DeliveryState,
} from "./canonical-imports.js";

// Re-export everything the DO needs through this single boundary file.
export {
  RELAY_PROTOCOL_VERSION,
  MAX_WORKSTATION_ID_LEN,
  MAX_BOOT_ID_LEN,
  MAX_REQUEST_ID_LEN,
  MAX_OPERATION_LEN,
  MAX_LINK_VERSION_LEN,
  MAX_CAPABILITIES,
  MAX_CAPABILITY_LEN,
  MAX_IDEMPOTENCY_KEY_LEN,
  MIN_TIMEOUT_MS,
  MAX_TIMEOUT_MS,
  DEFAULT_MAX_FRAME_BYTES,
  ID_GRAMMAR,
  MESSAGE_KINDS,
  CORRELATED_KINDS,
  isValidIdentifier,
  checkWorkstationId,
  checkRequestId,
  checkBootId,
  validateRelayMessage,
  parseRelayFrame,
  checkPayloadBudget,
  checkFrameBytes,
  utf8ByteLength,
};
export type {
  HelloMessage,
  HelloAckMessage,
  HeartbeatMessage,
  StatusMessage,
  ToolRequestMessage,
  ToolResultMessage,
  ToolErrorMessage,
  CancelMessage,
  CancelAckMessage,
  RelayMessage,
  RelayCheck,
  RelayValidationOptions,
  DeliveryState,
};

/**
 * Known kinds that the link may send AFTER a validated hello. The edge
 * rejects anything else (except pre-hello: only "hello" is accepted).
 */
export const POST_HELLO_KINDS: ReadonlySet<string> = new Set([
  "heartbeat",
  "status",
  "tool_result",
  "tool_error",
  "cancel_ack",
]);

/**
 * Edge-to-link outbound kinds. The edge sends these; the link must accept them.
 */
export const EDGE_OUTBOUND_KINDS: ReadonlySet<string> = new Set([
  "hello_ack",
  "tool_request",
  "cancel",
  "status",
]);

/** Strictly validate a hello against protocol v1 (pure, used by DO before
 *  writing state). Returns the validated message when ok. */
export function validateHello(hello: unknown): { ok: true; message: HelloMessage } | { ok: false; reason: string } {
  return validateCanonicalMessage(hello, "hello");
}

/** Validate that a generic inbound message is the expected canonical kind. */
export function validateCanonicalMessage(
  value: unknown,
  expectedKind: string,
): { ok: true; message: any } | { ok: false; reason: string } {
  const check = validateRelayMessage(value, { strictUnknownFields: true });
  if (!check.ok) return { ok: false, reason: `validation failed: ${check.code} ${check.reason}` };
  const msg = value as RelayMessage;
  if (msg.kind !== expectedKind) {
    return { ok: false, reason: `expected kind '${expectedKind}' but got '${msg.kind}'` };
  }
  return { ok: true, message: msg };
}

/** Encode a relay wire frame and enforce the byte budget BEFORE writing. */
export function encodeWire(msg: RelayMessage, maxBytes: number): { ok: true; text: string; bytes: number } | { ok: false; reason: string } {
  let text: string;
  try {
    text = JSON.stringify(msg);
  } catch {
    return { ok: false, reason: "relay encode failed" };
  }
  const bytes = utf8ByteLength(text);
  if (bytes > maxBytes) {
    return { ok: false, reason: `relay frame exceeds ${maxBytes} byte budget (${bytes})` };
  }
  return { ok: true, text, bytes };
}

/**
 * Parse and validate an inbound relay frame. Uses the canonical
 * parseRelayFrame (byte gate → JSON parse → strict validation). This is the
 * only entry point for untrusted wire frames.
 */
export function decodeWire(raw: string): { ok: true; message: RelayMessage } | { ok: false; code: string; reason: string } {
  const result = parseRelayFrame(raw, { strictUnknownFields: true });
  if (!result.ok) return { ok: false, code: result.code, reason: result.reason };
  return { ok: true, message: result.message };
}

/** Build a ResumeSummary for hello_ack from a pending request. */
export function buildResumeSummary(
  requestId: string,
  operation: string,
  state: "queued" | "sent" | "settled",
  deadlineMs: number,
): ResumeSummary {
  return { request_id: requestId, operation, state, deadline_ms: deadlineMs };
}

export interface ResumeSummary {
  request_id: string;
  operation: string;
  state: "queued" | "sent" | "settled";
  deadline_ms: number;
}