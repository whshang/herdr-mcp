/**
 * Local types for the workstation-side `herdr-link` (self-upgrade Phase 2).
 *
 * `herdr-link` is an independent, long-lived client that keeps a workstation
 * reachable by the relay edge through an outbound authenticated WebSocket.
 * This module only defines the workstation boundary: internal request
 * objects, the injected local-runtime transport interface, connection state,
 * and options. It performs no network I/O itself and has no Cloudflare imports.
 *
 * ─────────────────────────────────────────────────────────────────────────────
 * RELAY PROTOCOL INTEGRATION POINT (implemented)
 * ─────────────────────────────────────────────────────────────────────────────
 * The edge-to-workstation Relay Protocol v1 is canonical in `src/relay/**`
 * (protocol.ts / validation.ts). The raw WebSocket wire MUST use canonical
 * frames (protocol_version=1, workstation_id, and kinds hello / hello_ack /
 * heartbeat / status / tool_request / tool_result / tool_error / cancel /
 * cancel_ack). The old provisional wire fields (v/type/request/response/
 * ping/pong/runtime_status/drain/shutdown/error) are gone from the wire.
 *
 * `relay-adapter.ts` is the single place that:
 *   - builds canonical outbound frames from internal LinkRuntimeTransport
 *     shapes (encode* helpers), and
 *   - translates canonical inbound frames back into the internal request
 *     objects below (toInternalRequest / toInternalCancel).
 *
 * The LinkRuntimeTransport boundary here keeps its stable internal shapes
 * (`ToolRequestFrame`, `CancelFrame`) — those are LOCAL-only objects, never
 * serialized verbatim to the socket. The socket is the canonical contract.
 *
 * Do not move these definitions into another worker's scope while that scope
 * is undeveloped; edit them here and reconcile at integration time.
 * ─────────────────────────────────────────────────────────────────────────────
 */

/** Unique id for one boot of the sidecar process. */
export type BootId = string;
/** Fresh id per WSS connection attempt (also used for fast edge routing). */
export type ConnectionId = string;
/** Globally unique tool-request correlation id, opaque to this process. */
export type RequestId = string;

/** Machine-readable description of the socket lifecycle phase. */
export type ConnectionPhase =
  | "idle" // not started
  | "connecting" // socket being opened
  | "handshake" // socket open, waiting for hello_ack
  | "online" // registered; requests/heartbeats flowing
  | "reconnecting" // socket dropped, backoff wait before next attempt
  | "closing" // graceful shutdown in progress
  | "closed"; // permanently stopped (or never started)

/**
 * INTERNAL request object handed to LinkRuntimeTransport. This is NOT a wire
 * frame: the canonical wire shape is ToolRequestMessage in src/relay. The
 * `v`/`type` fields are retained for transport-boundary compatibility only
 * and are never serialized to the socket.
 */
export interface ToolRequestFrame {
  /** @deprecated local-only; canonical wire uses protocol_version */
  v: number;
  /** @deprecated local-only; canonical wire uses kind: "tool_request" */
  type: "request";
  workstation_id: string;
  request_id: RequestId;
  operation: string; // tool name, e.g. "herdr_inspect"
  arguments?: Record<string, unknown>;
  timeout_ms?: number; // budget hint, clamped locally
  contract_epoch?: number;
  contract_hash?: string;
  idempotency_key?: string;
  trace?: Record<string, unknown>;
}

/**
 * INTERNAL cancellation object handed to LinkRuntimeTransport. Local-only;
 * canonical wire shape is CancelMessage in src/relay.
 */
export interface CancelFrame {
  /** @deprecated local-only; canonical wire uses protocol_version */
  v: number;
  /** @deprecated local-only; canonical wire uses kind: "cancel" */
  type: "cancel";
  workstation_id: string;
  request_id: RequestId;
  reason?: string;
}

/** Internal final status used in LinkEventMap.request_finished only. */
export type ResponseFinalStatus = "ok" | "error" | "cancelled" | "timeout";

/** Snapshot of the local runtime identity advertised in hello/status. */
export interface RuntimeIdentitySnapshot {
  runtime_version: string;
  runtime_commit: string | null;
  runtime_generation: string | null;
  contract_epoch: number;
  contract_hash: string | null;
  herdr_version: string | null;
  herdr_protocol: string | null;
}

/** Discriminated union for what the local transport can return. */
export type RuntimeToolResult =
  | { ok: true; result: unknown }
  | { ok: false; code: string; retryable: boolean; error: { message: string; details?: unknown } };

/**
 * Injected local-runtime transport boundary.
 *
 * The sidecar does NOT inspect Herdr internals; it asks `transport` to run
 * tool requests and report identity/health. Implementations may back this by a
 * unix-socket RPC, HTTP, or an in-process adapter — the client only depends on
 * this contract, keeping the relay/edge direction provider-neutral.
 */
export interface LinkRuntimeTransport {
  readonly name: string;
  getRuntimeInfo(): Promise<RuntimeIdentitySnapshot> | RuntimeIdentitySnapshot;
  /** Best-effort invoke of one relay tool request. */
  dispatchRequest(req: ToolRequestFrame): Promise<RuntimeToolResult>;
  /** Best-effort signal to the runtime that a request should stop. */
  cancelRequest(requestId: RequestId, reason: string): Promise<void>;
  getHealth(): Promise<{ healthy: boolean; details?: string }> | { healthy: boolean; details?: string };
}

/**
 * Minimal evented WebSocket surface — the cloud-agnostic socket boundary.
 * Node.js `globalThis.WebSocket` (stable in >= 22) satisfies this; fake
 * sockets in tests implement only this small surface.
 */
export interface LinkWebSocket {
  readonly readyState: number;
  send(data: string): void;
  close(code?: number, reason?: string): void;
  /** Force-close without close handshake (stale-socket recovery). */
  terminate?(): void;
  addEventListener(type: string, listener: (event: any) => void): void;
  removeEventListener(type: string, listener: (event: any) => void): void;
}

/** Injectable clock/RNG to keep reconnect/backoff deterministic in tests. */
export type MillisFn = () => number;
export type RngFn = () => number;

export interface BackoffOptions {
  baseMs?: number; // first non-0 delay floor (default 1000)
  maxMs?: number; // hard per-attempt cap (default 60_000)
  factor?: number; // exponent base (default 2)
  jitter?: number; // 0..1 fraction cap of the window (default 1 = full jitter)
  rng?: RngFn;
}

export interface LinkEventMap {
  connecting: { attempt: number };
  connected: { connected_at_ms: number };
  disconnected: { code?: number; reason?: string };
  reconnect_scheduled: { attempt: number; delay_ms: number; reconnect_at_ms: number };
  handshake_failed: { message: string; code?: string };
  fatal: { message: string };
  request_started: { request_id: RequestId };
  request_finished: { request_id: RequestId; final_status: ResponseFinalStatus; code: string; retryable: boolean };
  heartbeat_sent: { at_ms: number };
  closed: unknown;
}

export type LinkEvent = { type: keyof LinkEventMap } & LinkEventMap[keyof LinkEventMap];

export interface LinkLogger {
  debug?(msg: string, ...rest: unknown[]): void;
  info?(msg: string, ...rest: unknown[]): void;
  warn?(msg: string, ...rest: unknown[]): void;
  error?(msg: string, ...rest: unknown[]): void;
}

/** Runtime + connection + counters; the `getStatus()` snapshot surface. */
export interface LinkStatus {
  static_url: string;
  workstation_id: string;
  boot_id: BootId;
  connection_id: string | null;
  protocol_version: number;
  link_version: string;
  phase: ConnectionPhase;
  stopped: boolean;
  connected_at_ms: number | null;
  last_edge_seen_ms: number | null;
  last_heartbeat_ms: number | null;
  reconnect_attempt: number;
  reconnect_at_ms: number | null;
  active_requests: number;
  max_pending: number;
  frames_sent: number;
  frames_received: number;
  malformed_frames: number;
  queue_overflow_responses: number;
  payload_too_large_rejected: number;
  timeouts_sent: number;
  runtime: RuntimeIdentitySnapshot | null;
  runtime_healthy: boolean;
  runtime_health_details?: string | null;
  last_error: string | null;
  fatal_error: string | null;
}

export type LinkExitKind =
  | "stopped"
  | "auth_rejected"
  | "contract_rejected"
  | "superseded"
  | "max_reconnect"
  | "fatal_error";

export interface LinkExitInfo {
  kind: LinkExitKind;
  message: string | null;
}

/**
 * herdr-link client options. All timings/limits are in ms.
 * `socketFactory`, `clock` and `rng` are the deterministic-levers for tests.
 */
export interface HerdrLinkOptions {
  workstationId: string;
  /** Full WSS edge URL; `workstation_id`/`link_token` are appended if absent. */
  edgeUrl: string;
  /** Workstation credential the edge validates on the WSS handshake. */
  linkToken: string;
  linkVersion?: string;
  transport: LinkRuntimeTransport;
  socketFactory?: (url: string) => LinkWebSocket;
  /** Subprotocol string offered on connect (default "herdr-link.v1"). */
  protocolId?: string;
  heartbeatMs?: number;
  handshakeTimeoutMs?: number;
  requestTimeoutMs?: number;
  /** Safe cap for concurrently in-flight tool requests. */
  maxPending?: number;
  /** Per-message payload budget (bytes, JSON text). */
  maxFrameBytes?: number;
  /** Treat a connection stale if no frames arrive in this window. */
  maxSilenceMs?: number;
  backoff?: BackoffOptions;
  drainMs?: number;
  maxReconnectAttempts?: number;
  clock?: MillisFn;
  rng?: RngFn;
  logger?: LinkLogger;
  onEvent?: (ev: LinkEvent) => void;
}