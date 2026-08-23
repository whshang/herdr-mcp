/**
 * errors.ts — Relay Protocol v1 error taxonomy and delivery-state classifier.
 *
 * Provider-independent. The edge answers MCP-facing tools/call failures and
 * the workstation reports tool outcomes with stable machine codes; the
 * delivery classifier below decides RETRYABILITY from what is actually known
 * about delivery, never from hope.
 *
 * Delivery state (what the transport knows about one request):
 *   `not_delivered`     — confirmed never forwarded to the runtime (edge
 *                         rejected offline, queue full, link never received
 *                         it). Safe to retry.
 *   `delivery_unknown`  — may have reached the runtime (sent then connection
 *                         dropped before any response, or timeout). Retryable
 *                         ONLY for read/idempotent operations.
 *   `delivered`         — runtime accepted and answered; retryability comes
 *                         from the error code, not from transport ambiguity.
 *
 * Retry safety class of an operation:
 *   `read`      — safe to repeat; repeated execution has no side effects.
 *   `idempotent`— mutating but carries an idempotency key / is safe to
 *                 deduplicate by the edge.
 *   `unsafe`    — mutating without idempotency guarantees; never blind-retry.
 */

import type { DeliveryState, RelayMessage, ToolErrorMessage } from "./protocol.js";

export type { DeliveryState } from "./protocol.js";
export type RetrySafety = "read" | "idempotent" | "unsafe";

/** Stable delivery-state codes surfaced on tool_error frames. */
export const DELIVERY_CODES: Record<DeliveryState, string> = {
  not_delivered: "not_delivered",
  delivery_unknown: "delivery_unknown",
  delivered: "delivered",
};

/**
 * Classify retryability from delivery evidence + operation safety.
 *
 * Rules (conservative: when in doubt, do not blind-retry a mutation):
 *   - not_delivered  → always retryable (nothing reached the runtime).
 *   - delivery_unknown → retryable only for read or idempotent ops.
 *   - delivered      → NOT retryable on the basis of delivery alone; the
 *                      caller should decide from the concrete error code.
 */
export function classifyRetryable(delivery: DeliveryState, safety: RetrySafety): boolean {
  switch (delivery) {
    case "not_delivered":
      return true;
    case "delivery_unknown":
      return safety === "read" || safety === "idempotent";
    case "delivered":
      return false;
  }
}

/** Field-level reason accompanying a classification decision. */
export function classifyReason(delivery: DeliveryState, safety: RetrySafety): string {
  switch (delivery) {
    case "not_delivered":
      return "request never reached the runtime; safe to retry";
    case "delivery_unknown":
      return safety === "unsafe"
        ? "delivery unknown and operation is mutating — do not blind-retry"
        : "delivery unknown but operation is safe to repeat";
    case "delivered":
      return "runtime answered; retry solely from the concrete error code";
  }
}

export interface ClassifiedDelivery {
  delivery: DeliveryState;
  retryable: boolean;
  reason: string;
}

/** Full classification result for error reporting. */
export function classifyDelivery(delivery: DeliveryState, safety: RetrySafety): ClassifiedDelivery {
  return {
    delivery,
    retryable: classifyRetryable(delivery, safety),
    reason: classifyReason(delivery, safety),
  };
}

// Error taxonomy for TOOL_ERROR (stable machine codes matching the failure
// model in docs/_wip/HERDR_MCP_SELF_UPGRADE_PLAN.md §14 and the edge scope's
// RelayErrorResult codes).

export type RelayErrorCode =
  | "workstation_offline"
  | "workstation_reconnecting"
  | "workstation_draining"
  | "request_timeout"
  | "delivery_uncertain"
  | "payload_too_large"
  | "bad_request"
  | "bad_operation"
  | "unsupported_protocol_version"
  | "workstation_mismatch"
  | "contract_mismatch"
  | "edge_capacity_exceeded"
  | "link_auth_failed"
  | "queue_full"
  | "request_rejected"
  | "cancelled"
  | "runtime_error"
  | "transport_error"
  | "internal_error";

export interface RelayError {
  code: RelayErrorCode;
  retryable: boolean;
  message?: string;
  request_id?: string;
  details?: unknown;
  delivery_state?: DeliveryState;
}

/** Build a tool_error wire message from relay error fields + correlation. */
export function toToolError(
  err: RelayError,
  base: { workstation_id: string; request_id: string; served_at_ms?: number },
): ToolErrorMessage {
  return {
    protocol_version: 1,
    kind: "tool_error",
    workstation_id: base.workstation_id,
    request_id: base.request_id,
    code: err.code,
    message: err.message,
    details: err.details,
    retryable: err.retryable,
    delivery_state: err.delivery_state,
    served_at_ms: base.served_at_ms,
  };
}

// Convenience constructors with sensible defaults, mirroring the edge scope's
// errorResult() helpers so the two trees stay mentally aligned.

export interface ErrorOpts {
  message?: string;
  requestId?: string;
  workstationId?: string;
  atMs?: number;
  details?: unknown;
}

function base(opts: ErrorOpts): Partial<RelayError> {
  return {
    message: opts.message,
    request_id: opts.requestId,
    details: opts.details,
  };
}

/** Workstation is not connected (never was, or offline). Safe to retry. */
export function offlineError(opts: ErrorOpts = {}): RelayError {
  return {
    ...base(opts),
    code: "workstation_offline",
    retryable: true,
    delivery_state: "not_delivered",
    message: opts.message ?? "workstation is offline",
  };
}

/** Link is connecting/reconnecting; nothing was delivered. Safe to retry. */
export function reconnectingError(opts: ErrorOpts = {}): RelayError {
  return {
    ...base(opts),
    code: "workstation_reconnecting",
    retryable: true,
    delivery_state: "not_delivered",
    message: opts.message ?? "workstation link is reconnecting; request not delivered",
  };
}

/** Planned drain in progress; retryable after drain completes. */
export function drainingError(opts: ErrorOpts = {}): RelayError {
  return {
    ...base(opts),
    code: "workstation_draining",
    retryable: true,
    delivery_state: "not_delivered",
    message: opts.message ?? "workstation link is draining; request not delivered",
  };
}

/**
 * Deadline exceeded; runtime execution state unknown from the edge's
 * perspective. Retryable only when the operation is a read (or the caller
 * brings its own idempotency).
 */
export function timeoutError(opts: ErrorOpts & { safety?: RetrySafety } = {}): RelayError {
  const safety = opts.safety ?? "unsafe";
  const retryable = classifyRetryable("delivery_unknown", safety);
  return {
    ...base(opts),
    code: "request_timeout",
    retryable,
    delivery_state: "delivery_unknown",
    message: retryable
      ? "request exceeded its deadline; operation is safe to repeat"
      : "request exceeded its deadline; outcome unknown — do not blindly retry a mutating op",
  };
}

/**
 * Connection ambiguity after the request was (or may have been) sent. The
 * retryable flag is derived from the operation safety, not from a boolean.
 */
export function uncertainError(opts: ErrorOpts & { safety?: RetrySafety } = {}): RelayError {
  const safety = opts.safety ?? "unsafe";
  const retryable = safety === "read" || safety === "idempotent";
  return {
    ...base(opts),
    code: "delivery_uncertain",
    retryable,
    delivery_state: "delivery_unknown",
    message: retryable
      ? "delivery outcome unknown; operation is safe to repeat"
      : "delivery outcome unknown; inspect workstation state before retrying a mutating op",
  };
}

/** Edge capacity (pending registry full). Retryable after backpressure. */
export function capacityError(opts: ErrorOpts = {}): RelayError {
  return {
    ...base(opts),
    code: "edge_capacity_exceeded",
    retryable: true,
    delivery_state: "not_delivered",
    message: opts.message ?? "edge pending-request capacity exceeded; retry later",
  };
}

/** A completed/cancelled/failed tool error produced by the runtime itself. */
export function deliveredError(opts: ErrorOpts & { code?: RelayErrorCode } = {}): RelayError {
  return {
    ...base(opts),
    code: opts.code ?? "runtime_error",
    retryable: false,
    delivery_state: "delivered",
    message: opts.message ?? "runtime reported an error",
  };
}

/** Request rejected before execution for structural/validation reasons. */
export function rejectedError(opts: ErrorOpts = {}): RelayError {
  return {
    ...base(opts),
    code: "request_rejected",
    retryable: false,
    delivery_state: "not_delivered",
    message: opts.message ?? "request rejected before delivery",
  };
}

/** Map a validation rejection code to the closest relay tool_error code. */
export function fromValidationCode(code: string): RelayErrorCode {
  switch (code) {
    case "frame_too_large":
    case "payload_too_large":
      return "payload_too_large";
    case "unsupported_protocol_version":
      return "unsupported_protocol_version";
    case "not_json":
    case "not_object":
    case "unknown_field":
    case "unknown_kind":
    case "missing_*":
    case "invalid_*":
      return "bad_request";
    default:
      return "bad_request";
  }
}

/** Compile-time shape guard used by consumers that pattern-match messages. */
export type AnyRelayMessage = RelayMessage;