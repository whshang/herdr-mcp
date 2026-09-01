/**
 * errors.ts — structured error taxonomy + retry/ambiguity classification.
 *
 * These codes are the ones the edge hands back to MCP-facing callers and to
 * the workstation link. They mirror the failure model in the plan (§14) and
 * the Relay Protocol delivery rules (§6): the edge never blindly replays a
 * mutating request after connection ambiguity.
 */

import type { OpClass } from "./limits.js";

export type RelayErrorCode =
  | "workstation_offline"
  | "workstation_reconnecting"
  | "workstation_draining"
  | "runtime_generation_superseded_before_dispatch"
  | "runtime_unavailable"
  | "request_timeout"
  | "delivery_uncertain"
  | "payload_too_large"
  | "bad_request"
  | "unsupported_protocol_version"
  | "workstation_mismatch"
  | "link_auth_failed"
  | "edge_capacity_exceeded"
  | "edge_mcp_placeholder"
  | "internal_error";

export interface RelayErrorResult {
  ok: false;
  code: RelayErrorCode;
  retryable: boolean;
  message?: string;
  requestId?: string;
  workstationId?: string;
  atMs?: number;
  details?: unknown;
  delivery_state?: string;
  retry_after_ms?: number;
  recovery?: RecoveryPolicy;
}

export interface RecoveryPolicy {
  action: "retry_read_only_probe";
  probe_tool: "herdr_inspect";
  max_attempts: number;
  backoff_ms: number[];
  mutation_replay: "only_after_not_delivered_or_verified_not_applied";
}

export const WORKSTATION_OFFLINE_RETRY_BACKOFF_MS = [5_000, 10_000, 20_000] as const;

function workstationOfflineRecovery(): RecoveryPolicy {
  return {
    action: "retry_read_only_probe",
    probe_tool: "herdr_inspect",
    max_attempts: WORKSTATION_OFFLINE_RETRY_BACKOFF_MS.length,
    backoff_ms: [...WORKSTATION_OFFLINE_RETRY_BACKOFF_MS],
    mutation_replay: "only_after_not_delivered_or_verified_not_applied",
  };
}

export function errorResult(
  code: RelayErrorCode,
  opts: {
    retryable?: boolean;
    message?: string;
    requestId?: string;
    workstationId?: string;
    atMs?: number;
    delivery_state?: string;
    retry_after_ms?: number;
    recovery?: RecoveryPolicy;
  } = {},
): RelayErrorResult {
  return {
    ok: false,
    code,
    retryable: opts.retryable ?? false,
    ...(opts.message !== undefined ? { message: opts.message } : {}),
    ...(opts.requestId !== undefined ? { requestId: opts.requestId } : {}),
    ...(opts.workstationId !== undefined ? { workstationId: opts.workstationId } : {}),
    ...(opts.atMs !== undefined ? { atMs: opts.atMs } : {}),
    ...(opts.delivery_state !== undefined ? { delivery_state: opts.delivery_state } : {}),
    ...(opts.retry_after_ms !== undefined ? { retry_after_ms: opts.retry_after_ms } : {}),
    ...(opts.recovery !== undefined ? { recovery: opts.recovery } : {}),
  };
}

/** Workstation is not connected (never was, or offline). Safe to retry. */
export function offlineResult(opts: { requestId?: string; workstationId?: string; atMs?: number } = {}) {
  return errorResult("workstation_offline", {
    retryable: true,
    delivery_state: "not_delivered",
    retry_after_ms: WORKSTATION_OFFLINE_RETRY_BACKOFF_MS[0],
    recovery: workstationOfflineRecovery(),
    message: "workstation is offline; request was not delivered",
    ...opts,
  });
}

/** Link is connecting/reconnecting; nothing was delivered yet. Safe to retry. */
export function reconnectingResult(opts: { requestId?: string; workstationId?: string; atMs?: number } = {}) {
  return errorResult("workstation_reconnecting", {
    retryable: true,
    delivery_state: "not_delivered",
    retry_after_ms: WORKSTATION_OFFLINE_RETRY_BACKOFF_MS[0],
    recovery: workstationOfflineRecovery(),
    message: "workstation link is reconnecting; request was not delivered",
    ...opts,
  });
}

/** Planned drain in progress. Retryable after drain completes. */
export function drainingResult(opts: { requestId?: string; workstationId?: string; atMs?: number } = {}) {
  return errorResult("workstation_draining", {
    retryable: true,
    message: "workstation link is draining; request not delivered",
    ...opts,
  });
}

/**
 * Deadline exceeded. Whether the runtime executed the op is unknown from the
 * edge's perspective: reads may be retried, mutating ops must NOT be blindly
 * retried (the caller should inspect before replay).
 */
export function timeoutResult(opts: { requestId?: string; workstationId?: string; atMs?: number; opClass?: OpClass } = {}) {
  const retryable = opts.opClass === "read";
  return errorResult("request_timeout", {
    retryable,
    message: retryable
      ? "request exceeded its deadline; retrying a read is safe"
      : "request exceeded its deadline; outcome unknown — do not blindly retry a mutating op",
    ...opts,
  });
}

/**
 * Request was (or may have been) delivered when the connection dropped.
 * Mutation outcome is ambiguous: NOT retryable. Read ops are classified
 * retryable by the caller path in classifyAmbiguousDelivery.
 */
export function uncertainResult(opts: { requestId?: string; workstationId?: string; atMs?: number } = {}) {
  return errorResult("delivery_uncertain", {
    retryable: false,
    message: "delivery outcome unknown after connection loss; inspect workstation state before retrying a mutating op",
    ...opts,
  });
}

/** Edge capacity (pending registry full). Retryable after backpressure. */
export function capacityResult(opts: { requestId?: string; workstationId?: string; atMs?: number } = {}) {
  return errorResult("edge_capacity_exceeded", {
    retryable: true,
    message: "edge pending-request capacity exceeded; retry later",
    ...opts,
  });
}

export interface CloseOutcome {
  code: RelayErrorCode;
  retryable: boolean;
}

/**
 * Map an opaque error code reported by the workstation link into the edge
 * taxonomy. Unknown strings degrade to internal_error (never surfaces raw).
 */
export function mapLinkErrorCode(code: string): RelayErrorCode {
  switch (code) {
    case "workstation_offline":
    case "workstation_reconnecting":
    case "workstation_draining":
    case "runtime_generation_superseded_before_dispatch":
    case "runtime_unavailable":
    case "request_timeout":
    case "delivery_uncertain":
    case "payload_too_large":
    case "bad_request":
    case "unsupported_protocol_version":
    case "workstation_mismatch":
    case "link_auth_failed":
    case "edge_capacity_exceeded":
    case "edge_mcp_placeholder":
    case "internal_error":
      return code;
    case "local_mcp_unreachable":
    case "local_mcp_http_error":
      return "runtime_unavailable";
    case "local_mcp_timeout":
      return "request_timeout";
    case "local_mcp_request_too_large":
    case "local_mcp_response_too_large":
      return "payload_too_large";
    case "local_mcp_bad_request":
      return "bad_request";
    default:
      return "internal_error";
  }
}

/**
 * Classify what a connection drop means for one in-flight request, based only
 * on persisted state (was it sent at all? what class of op is it?).
 */
export function classifyAmbiguousDelivery(state: "queued" | "sent", opClass: OpClass): CloseOutcome {
  if (state === "queued") {
    // Never left the edge -> reconnecting, safe to retry.
    return { code: "workstation_reconnecting" as const, retryable: true };
  }
  // Sent and connection dropped before a response.
  if (opClass === "read") {
    return { code: "delivery_uncertain" as const, retryable: true };
  }
  // Sent, mutating/unknown -> ambiguous mutation.
  return { code: "delivery_uncertain" as const, retryable: false };
}