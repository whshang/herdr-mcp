/**
 * protocol.ts — Relay Protocol v1 canonical envelope and message union.
 *
 * This is the provider-independent wire contract between a relay edge
 * (Cloudflare Worker / DO today, future VPS) and the workstation `herdr-link`
 * sidecar. It deliberately does NOT import anything from `src/link/**` or
 * `edge/cloudflare/**`: those scopes own their provisional transport types and
 * are reconciled to this canonical schema at integration time (mapping in
 * src/link/types.ts RELAY PROTOCOL INTEGRATION POINT).
 *
 * Wire design (see docs/_wip/HERDR_MCP_SELF_UPGRADE_PLAN.md §6):
 * - `protocol_version` is the number `1` on every message. A message with an
 *   unsupported version is rejected before any state is written.
 * - `kind` discriminates the message; the canonical set is:
 *   hello, hello_ack, heartbeat, status, tool_request, tool_result,
 *   tool_error, cancel, cancel_ack.
 * - `workstation_id` appears on every message (identity binding).
 * - `request_id` is required ONLY on correlated kinds (tool_request,
 *   tool_result, tool_error, cancel, cancel_ack). Control messages carrying a
 *   request_id fail strict validation (unknown field).
 * - `boot_id` identifies one link process boot and appears on hello,
 *   heartbeat and status.
 * - `runtime_generation` identifies the runtime generation that served a
 *   result/error and appears in status/tool_result/tool_error.
 * - Contract metadata: the edge sends the epoch/hash it expects in
 *   tool_request so the workstation can refuse mismatched contracts; the
 *   workstation advertises its runtime contract metadata in hello/status.
 *
 * Validation (types/limits) lives in validation.ts; canonical encoding in
 * canonical-json.ts; error/delivery taxonomy in errors.ts; contract manifest
 * hashing in contract.ts.
 */

/** Wire protocol version spoken by every Relay v1 participant. */
export const RELAY_PROTOCOL_VERSION = 1 as const;

/**
 * Delivery evidence a transport observed for one request (errors.ts classifies
 * retryability from this + operation safety).
 */
export type DeliveryState = "not_delivered" | "delivery_unknown" | "delivered";
/** Human-readable form used in diagnostics and rejected-version messages. */
export const RELAY_PROTOCOL_VERSION_STRING = "1";

export const MESSAGE_KINDS = [
  "hello",
  "hello_ack",
  "heartbeat",
  "status",
  "tool_request",
  "tool_result",
  "tool_error",
  "cancel",
  "cancel_ack",
] as const;
export type MessageKind = (typeof MESSAGE_KINDS)[number];

/** Kinds that must carry a request_id (correlated with one tool call). */
export const CORRELATED_KINDS: ReadonlySet<MessageKind> = new Set([
  "tool_request",
  "tool_result",
  "tool_error",
  "cancel",
  "cancel_ack",
]);

/** Every message requires protocol_version, kind and workstation_id. */
export interface RelayEnvelopeBase {
  protocol_version: typeof RELAY_PROTOCOL_VERSION;
  kind: MessageKind;
  workstation_id: string;
}

/**
 * Answer to a shape/validation check. Errors are values, not throws, so that
 * untrusted inbound frames can be rejected with a stable machine code on the
 * way in.
 */
export type RelayCheck =
  | { ok: true }
  | { ok: false; code: RelayValidationCode; reason: string };

/** Stable rejection codes used by the validation layer. */
export type RelayValidationCode =
  | "not_json"
  | "not_object"
  | "missing_protocol_version"
  | "unsupported_protocol_version"
  | "missing_kind"
  | "unknown_kind"
  | "missing_workstation_id"
  | "invalid_workstation_id"
  | "missing_request_id"
  | "invalid_request_id"
  | "unexpected_request_id"
  | "invalid_boot_id"
  | "invalid_id"
  | "invalid_string"
  | "invalid_number"
  | "invalid_boolean"
  | "invalid_enum"
  | "unknown_field"
  | "frame_too_large"
  | "too_deep"
  | "too_many_keys"
  | "too_many_items"
  | "string_too_long"
  | "payload_too_large"
  | "invalid_arguments"
  | "invalid_result"
  | "invalid_details"
  | "invalid_capabilities"
  | "invalid_runtime"
  | "invalid_operation";

/** Runtime/contract identity advertised by the workstation side. */
export interface RuntimeContractInfo {
  runtime_version: string;
  runtime_commit: string | null;
  runtime_generation: string | null;
  contract_epoch: number;
  contract_hash: string | null;
  herdr_version: string | null;
  herdr_protocol: string | null;
}

/** Workstation → edge: registration on every fresh connection. */
export interface HelloMessage extends RelayEnvelopeBase {
  kind: "hello";
  boot_id: string;
  link_version: string;
  /** Connection timestamp (ms epoch). */
  connected_at_ms?: number;
  /** Capability tokens advertised by the link (bounded). */
  capabilities: string[];
  runtime?: RuntimeContractInfo;
}

/** Reserved hello_ack failure codes (stable machine codes). */
export type HelloAckFailureCode =
  | "auth_rejected"
  | "auth_expired"
  | "session_invalid"
  | "protocol_incompatible"
  | "contract_mismatch"
  | "workstation_mismatch"
  | "internal_error";

/** Bounded resume summary the edge sends after a reconnect (no args, ever). */
export interface ResumeSummary {
  request_id: string;
  operation: string;
  /** Edge-side delivery state the link uses to reconcile. */
  state: "queued" | "sent" | "settled";
  deadline_ms: number;
}

/** Edge → workstation: registration outcome. */
export type HelloAckMessage = RelayEnvelopeBase &
  (
    | {
        kind: "hello_ack";
        ok: true;
        server_version?: string;
        edge_deployment_id?: string;
        capabilities?: string[];
        /** True when this ack re-attaches a surviving relationship. */
        reconnect?: boolean;
        /** Bounded resume summaries for in-flight requests (post-reconnect). */
        resume?: ResumeSummary[];
        /** request_ids for which a completion is already persisted. */
        completed?: string[];
      }
    | {
        kind: "hello_ack";
        ok: false;
        code: HelloAckFailureCode | string;
        message: string;
      }
  );

/** Workstation → edge: periodic presence + cheap runtime glimpse. */
export interface HeartbeatMessage extends RelayEnvelopeBase {
  kind: "heartbeat";
  boot_id: string;
  sent_at_ms: number;
  link_uptime_ms?: number;
  active_requests: number;
  runtime?: RuntimeContractInfo;
}

/**
 * Edge → workstation: request a runtime snapshot (query:true, minimal body),
 * or workstation → edge: full status report.
 */
export interface StatusMessage extends RelayEnvelopeBase {
  kind: "status";
  /** True on the edge→workstation query form (workstation answers with a
   *  full report). */
  query?: boolean;
  boot_id?: string;
  runtime?: RuntimeContractInfo;
  runtime_generation?: string | null;
  healthy?: boolean;
  health_details?: string | null;
  active_requests?: number;
  link_uptime_ms?: number;
  last_error?: string | null;
  sent_at_ms?: number;
}

/** Edge → workstation: one relayed tool invocation. */
export interface ToolRequestMessage extends RelayEnvelopeBase {
  kind: "tool_request";
  request_id: string;
  /** Tool name, e.g. "herdr_inspect". */
  operation: string;
  /** Validated arguments payload (bounded; the edge never logs values). */
  arguments?: Record<string, unknown>;
  /** Deadline budget hint in ms; the workstation clamps it locally. */
  timeout_ms?: number;
  /** Contract epoch/hash the EDGE expects; the workstation refuses on
   *  mismatch so a runtime upgrade can never silently change the ABI. */
  contract_epoch?: number;
  contract_hash?: string;
  /** Idempotency key for safely deduplicated mutating operations. */
  idempotency_key?: string;
  /** Bounded trace metadata; must contain no secrets or prompt content. */
  trace?: Record<string, unknown>;
}

/** Workstation → edge: successful outcome correlated by request_id. */
export interface ToolResultMessage extends RelayEnvelopeBase {
  kind: "tool_result";
  request_id: string;
  /** Bounded structured result. */
  result?: unknown;
  served_at_ms: number;
  runtime_generation?: string | null;
  /** Identity of the transport that served it (e.g. "herdr", "local"). */
  transport_name?: string | null;
}

/** Workstation → edge: failed outcome correlated by request_id. */
export interface ToolErrorMessage extends RelayEnvelopeBase {
  kind: "tool_error";
  request_id: string;
  /** Stable machine code from the error taxonomy (errors.ts) or runtime. */
  code: string;
  message?: string;
  details?: unknown;
  /** True when the edge may safely retry this operation. */
  retryable: boolean;
  /**
   * Delivery evidence the WORKSTATION observed. `not_delivered`/`delivered`
   * are confirmed; `delivery_unknown` means it may have executed and the
   * edge must not blindly replay mutating ops.
   */
  delivery_state?: DeliveryState;
  served_at_ms?: number;
  runtime_generation?: string | null;
}

/** Edge → workstation: cancellation of one in-flight request. */
export interface CancelMessage extends RelayEnvelopeBase {
  kind: "cancel";
  request_id: string;
  reason?: string;
}

/** Workstation → edge: acknowledgement/refusal of a cancel. */
export interface CancelAckMessage extends RelayEnvelopeBase {
  kind: "cancel_ack";
  request_id: string;
  /** True when the request was in flight and cancellation was accepted. */
  accepted: boolean;
  cancelled_at_ms: number;
  reason?: string | null;
}

export type RelayMessage =
  | HelloMessage
  | HelloAckMessage
  | HeartbeatMessage
  | StatusMessage
  | ToolRequestMessage
  | ToolResultMessage
  | ToolErrorMessage
  | CancelMessage
  | CancelAckMessage;