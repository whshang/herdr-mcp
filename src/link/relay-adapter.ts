/**
 * relay-adapter.ts — Canonical Relay v1 wire integration for herdr-link.
 *
 * Single integration point between src/link/** (workstation-side client) and
 * src/relay/** (canonical protocol). This module:
 *
 *   1. Re-exports `parseRelayFrame` and `RELAY_PROTOCOL_VERSION` so link code
 *      never imports src/relay directly.
 *   2. Provides encode helpers that build canonical RelayMessage values from
 *      internal link shapes.
 *   3. Provides `toInternalRequest` / `toInternalCancel` that translate
 *      canonical frames into the internal request objects that
 *      LinkRuntimeTransport expects.
 *   4. Provides `toRuntimeContractInfo` for the structural identity mapping.
 *   5. Provides `encodeCompactOversizedError` for the bounded outbound-error
 *      path.
 *
 * Wire mapping (old → canonical):
 *   v:1                          → protocol_version:1
 *   type: "hello"                → kind: "hello"
 *   type: "hello_ack"            → kind: "hello_ack"
 *   type: "request"              → kind: "tool_request"
 *   type: "response" (ok)        → kind: "tool_result"
 *   type: "response" (error)     → kind: "tool_error"
 *   type: "cancel"               → kind: "cancel"
 *   type: "cancel" response      → kind: "cancel_ack"
 *   type: "heartbeat"            → kind: "heartbeat"
 *   type: "ping" / "pong"        → removed (heartbeat replaces)
 *   type: "runtime_status" (q)   → kind: "status" (query:true)
 *   type: "runtime_status" (r)   → kind: "status" (report)
 *   type: "drain"                → removed (local lifecycle only)
 *   type: "shutdown"             → removed (close socket for graceful shutdown)
 *   type: "error"                → removed (hello_ack.ok:false covers auth)
 *
 * @module
 */

import {
  RELAY_PROTOCOL_VERSION,
  type CancelMessage,
  type HeartbeatMessage,
  type HelloAckMessage,
  type HelloMessage,
  type RelayMessage,
  type RuntimeContractInfo,
  type StatusMessage,
  type ToolErrorMessage,
  type ToolRequestMessage,
  type ToolResultMessage,
  type CancelAckMessage,
} from "../relay/protocol.js";
import { parseRelayFrame, type ParseResult, type RelayValidationOptions } from "../relay/validation.js";
import type {
  CancelFrame,
  RuntimeIdentitySnapshot,
  RuntimeToolResult,
  ToolRequestFrame,
} from "./types.js";

// Re-export the canonical parse entry point and version constant so link code
// never imports src/relay directly.
export { RELAY_PROTOCOL_VERSION, parseRelayFrame };
export type { ParseResult, RelayValidationOptions, ToolRequestMessage, CancelMessage, CancelAckMessage, ToolResultMessage, ToolErrorMessage, HelloMessage, HelloAckMessage, HeartbeatMessage, StatusMessage, RelayMessage, RuntimeContractInfo };

// ─────────────────────────────────────────────────────────────────────────────
// Identity mapping
// ─────────────────────────────────────────────────────────────────────────────

/**
 * Map the internal RuntimeIdentitySnapshot to the canonical RuntimeContractInfo.
 * This is a structural identity: both shapes have the same fields.
 */
export function toRuntimeContractInfo(rt: RuntimeIdentitySnapshot): RuntimeContractInfo {
  return {
    runtime_version: rt.runtime_version,
    runtime_commit: rt.runtime_commit,
    runtime_generation: rt.runtime_generation,
    contract_epoch: rt.contract_epoch,
    contract_hash: rt.contract_hash,
    herdr_version: rt.herdr_version,
    herdr_protocol: rt.herdr_protocol,
  };
}

// ─────────────────────────────────────────────────────────────────────────────
// Outbound canonical message builders
// ─────────────────────────────────────────────────────────────────────────────

/** Build a canonical hello message from link identity + runtime. */
export function encodeHelloMessage(
  workstationId: string,
  bootId: string,
  linkVersion: string,
  capabilities: string[],
  runtime: RuntimeIdentitySnapshot,
  sentAtMs: number,
): HelloMessage {
  return {
    protocol_version: RELAY_PROTOCOL_VERSION,
    kind: "hello",
    workstation_id: workstationId,
    boot_id: bootId,
    link_version: linkVersion,
    connected_at_ms: sentAtMs,
    capabilities,
    runtime: toRuntimeContractInfo(runtime),
  };
}

/** Build a canonical heartbeat message. */
export function encodeHeartbeatMessage(
  workstationId: string,
  bootId: string,
  activeRequests: number,
  runtime: RuntimeIdentitySnapshot,
  linkUptimeMs: number,
  sentAtMs: number,
): HeartbeatMessage {
  return {
    protocol_version: RELAY_PROTOCOL_VERSION,
    kind: "heartbeat",
    workstation_id: workstationId,
    boot_id: bootId,
    sent_at_ms: sentAtMs,
    link_uptime_ms: linkUptimeMs,
    active_requests: activeRequests,
    runtime: toRuntimeContractInfo(runtime),
  };
}

/** Build a canonical status report (workstation → edge). */
export function encodeStatusReport(
  workstationId: string,
  runtime: RuntimeIdentitySnapshot,
  healthy: boolean,
  healthDetails: string | null | undefined,
  activeRequests: number,
  linkUptimeMs: number,
  lastError: string | null | undefined,
  sentAtMs: number,
): StatusMessage {
  return {
    protocol_version: RELAY_PROTOCOL_VERSION,
    kind: "status",
    workstation_id: workstationId,
    query: false,
    runtime: toRuntimeContractInfo(runtime),
    runtime_generation: runtime.runtime_generation,
    healthy,
    health_details: healthDetails ?? null,
    active_requests: activeRequests,
    link_uptime_ms: linkUptimeMs,
    last_error: lastError ?? null,
    sent_at_ms: sentAtMs,
  };
}

/** Build a canonical tool_result message. */
export function encodeToolResultMessage(
  workstationId: string,
  requestId: string,
  result: unknown,
  servedAtMs: number,
  runtimeGeneration: string | null | undefined,
  transportName: string | null | undefined,
): ToolResultMessage {
  return {
    protocol_version: RELAY_PROTOCOL_VERSION,
    kind: "tool_result",
    workstation_id: workstationId,
    request_id: requestId,
    result,
    served_at_ms: servedAtMs,
    runtime_generation: runtimeGeneration ?? null,
    transport_name: transportName ?? null,
  };
}

/** Build a canonical tool_error message. */
export function encodeToolErrorMessage(
  workstationId: string,
  requestId: string,
  code: string,
  retryable: boolean,
  message: string,
  details?: unknown,
  deliveryState?: "not_delivered" | "delivery_unknown" | "delivered",
  servedAtMs?: number,
  runtimeGeneration?: string | null,
): ToolErrorMessage {
  return {
    protocol_version: RELAY_PROTOCOL_VERSION,
    kind: "tool_error",
    workstation_id: workstationId,
    request_id: requestId,
    code,
    message,
    details,
    retryable,
    delivery_state: deliveryState,
    served_at_ms: servedAtMs ?? Date.now(),
    runtime_generation: runtimeGeneration ?? null,
  };
}

/** Build a canonical cancel_ack message. */
export function encodeCancelAckMessage(
  workstationId: string,
  requestId: string,
  accepted: boolean,
  cancelledAtMs: number,
  reason?: string | null,
): CancelAckMessage {
  return {
    protocol_version: RELAY_PROTOCOL_VERSION,
    kind: "cancel_ack",
    workstation_id: workstationId,
    request_id: requestId,
    accepted,
    cancelled_at_ms: cancelledAtMs,
    reason: reason ?? null,
  };
}

// ─────────────────────────────────────────────────────────────────────────────
// Inbound canonical → internal translation
// ─────────────────────────────────────────────────────────────────────────────

/**
 * Translate a canonical ToolRequestMessage into the internal ToolRequestFrame
 * that LinkRuntimeTransport.dispatchRequest() expects. The internal shape
 * keeps the deprecated `v`/`type` fields but the canonical values are
 * mapped to the correct keys.
 */
export function toInternalRequest(msg: ToolRequestMessage): ToolRequestFrame {
  return {
    v: 1,
    type: "request",
    workstation_id: msg.workstation_id,
    request_id: msg.request_id,
    operation: msg.operation,
    arguments: msg.arguments,
    timeout_ms: msg.timeout_ms,
    contract_epoch: msg.contract_epoch,
    contract_hash: msg.contract_hash,
    idempotency_key: msg.idempotency_key,
    trace: msg.trace,
  };
}

/**
 * Translate a canonical CancelMessage into the internal CancelFrame that
 * LinkRuntimeTransport.cancelRequest() accepts.
 */
export function toInternalCancel(msg: CancelMessage): CancelFrame {
  return {
    v: 1,
    type: "cancel",
    workstation_id: msg.workstation_id,
    request_id: msg.request_id,
    reason: msg.reason,
  };
}

// ─────────────────────────────────────────────────────────────────────────────
// Compact oversized error (bounded outbound error when the real result is too
// large to fit the frame budget). Produces a canonical tool_error message.
// ─────────────────────────────────────────────────────────────────────────────

export function encodeCompactOversizedError(
  workstationId: string,
  requestId: string,
  runtimeGeneration: string | null | undefined,
): ToolErrorMessage {
  return {
    protocol_version: RELAY_PROTOCOL_VERSION,
    kind: "tool_error",
    workstation_id: workstationId,
    request_id: requestId,
    code: "response_too_large",
    message: "response exceeds maxFrameBytes; paginate the result",
    retryable: false,
    delivery_state: "delivered",
    served_at_ms: Date.now(),
    runtime_generation: runtimeGeneration ?? null,
  };
}