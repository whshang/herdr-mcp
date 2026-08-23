/**
 * herdr-link — workstation-side relay client (self-upgrade Phase 2 skeleton).
 *
 * Keeps the workstation attached to the relay edge with a single long-lived
 * outbound authenticated WebSocket, dispatching relay tool requests through an
 * injected `LinkRuntimeTransport` and returning results/errors correlated by
 * `request_id`.
 *
 * WIRE CONTRACT: the raw WebSocket JSON is the CANONICAL Relay Protocol v1
 * defined in src/relay/** (protocol.ts / validation.ts). This file has no
 * Cloudflare imports and never imports src/relay directly — it goes through
 * relay-adapter.ts, the single integration point that builds canonical
 * outbound frames and validates/translates inbound frames.
 *
 * Canonical wire kinds used here: hello, hello_ack, heartbeat, status,
 * tool_request, tool_result, tool_error, cancel, cancel_ack.
 * Old wire kinds (ping, pong, runtime_status, drain, shutdown, error,
 * request, response, v/type fields) are GONE from the wire. Heartbeat replaces
 * ping/pong; status(query:true) replaces the runtime_status query; graceful
 * shutdown closes the socket (no shutdown frame); cancellation answers with
 * cancel_ack. drain/upgrade-status are local lifecycle concerns only — the
 * canonical protocol deliberately has no wire kind for them.
 *
 * Runtime self-restart / runtime generation switching is intentionally NOT
 * implemented here (plan Phase 6); this client only keeps the control channel
 * alive and expects the transport to resolve tool requests.
 */

import { randomUUID } from "node:crypto";
import type {
  CancelAckMessage,
  CancelMessage,
  HelloAckMessage,
  HeartbeatMessage,
  RelayMessage,
  StatusMessage,
  ToolErrorMessage,
  ToolRequestMessage,
  ToolResultMessage,
} from "./relay-adapter.js";
import {
  RELAY_PROTOCOL_VERSION,
  encodeCancelAckMessage,
  encodeCompactOversizedError,
  encodeHeartbeatMessage,
  encodeHelloMessage,
  encodeStatusReport,
  encodeToolErrorMessage,
  encodeToolResultMessage,
  parseRelayFrame,
  toInternalCancel,
  toInternalRequest,
} from "./relay-adapter.js";
import {
  type CancelFrame,
  type ConnectionPhase,
  type HerdrLinkOptions,
  type LinkEvent,
  type LinkEventMap,
  type LinkExitInfo,
  type LinkLogger,
  type LinkRuntimeTransport,
  type LinkStatus,
  type LinkWebSocket,
  type RequestId,
  type ResponseFinalStatus,
  type RuntimeIdentitySnapshot,
  type RuntimeToolResult,
  type ToolRequestFrame,
} from "./types.js";
import { ExponentialBackoff } from "./backoff.js";
import { buildEdgeUrl, buildLinkProtocols, createStandardWebSocket } from "./socket.js";

/** Human-facing sidecar version advertised in hello. */
export const LINK_VERSION = "0.1.0";
/** WSS subprotocol the client offers on connect. */
export const LINK_SUBPROTOCOL = "herdr-link.v1";

export const LINK_DEFAULT_HEARTBEAT_MS = 30_000;
export const LINK_DEFAULT_HANDSHAKE_TIMEOUT_MS = 10_000;
export const LINK_DEFAULT_REQUEST_TIMEOUT_MS = 60_000;
export const LINK_DEFAULT_MAX_PENDING = 16;
export const LINK_DEFAULT_MAX_FRAME_BYTES = 262_144;
export const LINK_DEFAULT_MAX_SILENCE_MS = 90_000;
export const LINK_DEFAULT_DRAIN_MS = 5_000;
/** How long a runtime snapshot stays fresh before being re-queried. */
export const RUNTIME_CACHE_TTL_MS = 5_000;

/** WHATWG WebSocket readyState values (not exposed as constants in Node). */
export const WS_CONNECTING = 0;
export const WS_OPEN = 1;
export const WS_CLOSING = 2;
export const WS_CLOSED = 3;

export const WS_CLOSE_NORMAL = 1000;
export const WS_CLOSE_GOING_AWAY = 1001;
export const WS_CLOSE_POLICY = 1008;
/** Edge fencing: a newer authenticated link for the same workstation won. */
export const WS_CLOSE_SUPERSEDED = 4409;
/** Close codes that mean "stop reconnecting, the credential/workstation is
 *  refused" (4401/4403 are the conventional HTTP-style WS denial codes). */
export const AUTH_REJECT_CLOSE_CODES: ReadonlySet<number> = new Set([1008, 4401, 4403]);

const DEFAULT_CAPABILITIES: readonly string[] = [
  "relay.request",
  "relay.cancel",
  "relay.heartbeat",
  "relay.status",
];

/**
 * Stable machine codes this side emits for workstation-side failures. These
 * are carried on canonical tool_error frames as `code` (preserving
 * retryable / delivery_state / details semantics).
 */
export const LINK_CODE = {
  queueFull: "request_queue_full",
  payloadTooLarge: "payload_too_large",
  responseTooLarge: "response_too_large",
  requestTimeout: "request_timeout",
  cancelled: "cancelled",
  transportError: "transport_error",
  linkStopping: "link_stopping",
  duplicateRequest: "duplicate_request",
} as const;

type HandshakeOutcome =
  | { ok: true; serverVersion?: string; edgeDeploymentId?: string }
  | { ok: false; fatal: boolean; code: string; message: string };

type AttemptOutcome =
  | { kind: "dropped" }
  | { kind: "fatal"; exitKind: "auth_rejected" | "contract_rejected" | "superseded" | "fatal_error"; message: string };

/** Bookkeeping for one in-flight relay tool request. */
class PendingSlot {
  timer: ReturnType<typeof setTimeout> | null;
  cancelled = false;
  private settled = false;

  constructor(readonly req: ToolRequestFrame, readonly timeoutMs: number, readonly startedAtMs: number) {
    this.timer = null;
  }

  /** True only for the first caller to settle this slot. */
  claimSettle(): boolean {
    if (this.settled) return false;
    this.settled = true;
    return true;
  }
}

/** @internal */
export type NormalizedLinkOptions = {
  workstationId: string;
  edgeUrl: string;
  linkToken: string;
  linkVersion: string;
  protocolId: string;
  heartbeatMs: number;
  handshakeTimeoutMs: number;
  requestTimeoutMs: number;
  maxPending: number;
  maxFrameBytes: number;
  maxSilenceMs: number;
  drainMs: number;
  maxReconnectAttempts: number | null;
  socketFactory: (url: string) => LinkWebSocket;
  logger: LinkLogger;
};

export class HerdrLink {
  readonly workstationId: string;
  readonly transport: LinkRuntimeTransport;
  readonly opts: NormalizedLinkOptions;

  readonly bootId: ReturnType<typeof randomUUID>;
  /** Display form of the edge URL with the link token redacted. */
  readonly displayUrl: string;

  private readonly backoff: ExponentialBackoff;
  private readonly now: () => number;
  private readonly onEvent: ((ev: LinkEvent) => void) | undefined;
  private readonly rng: () => number;

  private ws: LinkWebSocket | null = null;
  private connectionId: string | null = null;
  private phase: ConnectionPhase = "idle";

  private stopped = false;
  private lastExit: LinkExitInfo | null = null;
  private loopExit: Promise<LinkExitInfo> | null = null;

  private connectedAtMs: number | null = null;
  private lastEdgeSeenMs: number | null = null;
  private lastHeartbeatMs: number | null = null;
  private reconnectAttempt = 0;
  private reconnectAtMs: number | null = null;

  private readonly pending = new Map<RequestId, PendingSlot>();
  private readonly startedAt: number;

  /** Cached runtime identity snapshot for cheap heartbeat/status frames. */
  private runtimeCache: RuntimeIdentitySnapshot | null = null;
  private runtimeCacheAtMs = 0;
  private healthCache: { healthy: boolean; details?: string } = { healthy: true };

  private lastError: string | null = null;
  private fatalError: string | null = null;

  // In-flight per-attempt waiters.
  private openResolve: ((ok: boolean) => void) | null = null;
  private closeResolve: ((code: number, reason: string) => void) | null = null;
  private handshakeTimeoutTimer: ReturnType<typeof setTimeout> | null = null;

  private heartbeatTimer: ReturnType<typeof setInterval> | null = null;
  private silenceTimer: ReturnType<typeof setInterval> | null = null;
  private forceCloseTimer: ReturnType<typeof setTimeout> | null = null;
  private sleepToken: { timer: ReturnType<typeof setTimeout>; resolve: (v: boolean) => void } | null = null;

  private counters: {
    framesSent: number;
    framesReceived: number;
    malformedFrames: number;
    socketErrors: number;
    queueOverflow: number;
    payloadTooLarge: number;
    timeouts: number;
  } = {
    framesSent: 0,
    framesReceived: 0,
    malformedFrames: 0,
    socketErrors: 0,
    queueOverflow: 0,
    payloadTooLarge: 0,
    timeouts: 0,
  };

  constructor(raw: HerdrLinkOptions) {
    if (!raw.workstationId || typeof raw.workstationId !== "string") {
      throw new TypeError("herdr-link: workstationId (string) is required");
    }
    if (!raw.transport || typeof raw.transport.dispatchRequest !== "function") {
      throw new TypeError("herdr-link: transport (LinkRuntimeTransport) is required");
    }
    let edge: URL;
    try {
      edge = new URL(raw.edgeUrl);
    } catch {
      throw new TypeError("herdr-link: edgeUrl must be an absolute wss:// URL");
    }
    if (edge.protocol !== "wss:" && edge.protocol !== "ws:") {
      throw new TypeError(`herdr-link: edgeUrl must be wss:// or ws:// (got "${edge.protocol}")`);
    }

    this.workstationId = raw.workstationId;
    this.transport = raw.transport;
    this.now = raw.clock ?? Date.now;
    this.rng = raw.rng ?? Math.random;
    this.onEvent = raw.onEvent;
    this.bootId = randomUUID();
    this.displayUrl = redactUrl(raw.edgeUrl);
    this.startedAt = this.now();

    const opts: NormalizedLinkOptions = {
      workstationId: raw.workstationId,
      edgeUrl: raw.edgeUrl,
      linkToken: raw.linkToken,
      linkVersion: raw.linkVersion ?? LINK_VERSION,
      protocolId: raw.protocolId ?? LINK_SUBPROTOCOL,
      heartbeatMs: clampRange(raw.heartbeatMs, LINK_DEFAULT_HEARTBEAT_MS, 10, 3_600_000),
      handshakeTimeoutMs: clampRange(raw.handshakeTimeoutMs, LINK_DEFAULT_HANDSHAKE_TIMEOUT_MS, 10, 60_000),
      requestTimeoutMs: clampRange(raw.requestTimeoutMs, LINK_DEFAULT_REQUEST_TIMEOUT_MS, 50, 300_000),
      maxPending: clampRange(raw.maxPending, LINK_DEFAULT_MAX_PENDING, 1, 4096),
      maxFrameBytes: clampRange(raw.maxFrameBytes, LINK_DEFAULT_MAX_FRAME_BYTES, 16, 16 * 1024 * 1024),
      maxSilenceMs: clampRange(raw.maxSilenceMs, LINK_DEFAULT_MAX_SILENCE_MS, 50, 3_600_000),
      drainMs: clampRange(raw.drainMs, LINK_DEFAULT_DRAIN_MS, 0, 120_000),
      maxReconnectAttempts: raw.maxReconnectAttempts == null ? null : clampRange(raw.maxReconnectAttempts, 0, 0, 1_000_000),
      socketFactory:
        raw.socketFactory ?? ((url) => createStandardWebSocket(
          url,
          buildLinkProtocols(raw.protocolId ?? LINK_SUBPROTOCOL, raw.linkToken),
        )),
      logger: raw.logger ?? {},
    };
    this.opts = opts;
    this.backoff = new ExponentialBackoff(raw.backoff ?? { rng: this.rng });
  }

/** Fire-and-forget connect for the sidecar style; failures surface in status. */
  start(): void {
    void this.connect().then(
      (exit) => {
        this.#log("warn", `herdr-link exited: ${exit.kind}${exit.message ? ` — ${exit.message}` : ""}`, {});
      },
      (err) => {
        // Defensive: a raw connect() rejection must never become an
        // unhandledRejection in the sidecar process.
        const message = err instanceof Error ? err.message : String(err);
        this.fatalError = message;
        this.#log("error", `herdr-link connect() crashed: ${message}`, {});
      },
    );
  }

  /**
   * Run until the link fully stops (graceful `close()` or fatal/auth refusal).
   * Safe to call multiple times — the underlying run is shared.
   */
  async connect(): Promise<LinkExitInfo> {
    if (this.loopExit) return this.loopExit;
    if (this.stopped) {
      this.phase = "closed";
      return { kind: "stopped", message: "closed before connect" };
    }
    const run = this.#runLoop();
    this.loopExit = run;
    const exit = await run;
    this.lastExit = exit;
    this.phase = "closed";
    this.#emit("closed", { exit });
    return exit;
  }

  /**
   * Graceful stop (canonical: NO shutdown frame — close the socket after
   * draining in-flight work, then reject leftovers locally).
   */
  async close(opts?: { reason?: string; drainMs?: number }): Promise<LinkExitInfo> {
    if (this.stopped) return this.lastExit ?? { kind: "stopped", message: opts?.reason ?? "link_shutdown" };
    const reason = opts?.reason ?? "link_shutdown";
    this.stopped = true;
    this.#log("info", `link close requested (${reason})`, {});
    this.#wakeSleep();
    this.phase = "closing";
    this.#clearIntervals();

    const ws = this.ws;
    if (ws && this.#socketIsOpen(ws)) {
      await this.#waitForPendingEmpty(opts?.drainMs ?? this.opts.drainMs);
      this.#beginClose(ws, WS_CLOSE_NORMAL, reason.slice(0, 120));
      this.#scheduleForceClose(2000);
    }

    // Leftovers cannot reach the edge anymore → settle them off with retryable error.
    this.#rejectAllPending(LINK_CODE.linkStopping, "link shutting down", true);

    if (this.loopExit) await this.loopExit;
    return this.lastExit ?? { kind: "stopped", message: reason };
  }

  /** Live snapshot of connection + runtime + counters (plan §13 status fields). */
  getStatus(): LinkStatus {
    return {
      static_url: this.displayUrl,
      workstation_id: this.workstationId,
      boot_id: this.bootId,
      connection_id: this.connectionId,
      protocol_version: RELAY_PROTOCOL_VERSION,
      link_version: this.opts.linkVersion,
      phase: this.phase,
      stopped: this.stopped,
      connected_at_ms: this.connectedAtMs,
      last_edge_seen_ms: this.lastEdgeSeenMs,
      last_heartbeat_ms: this.lastHeartbeatMs,
      reconnect_attempt: this.reconnectAttempt,
      reconnect_at_ms: this.reconnectAtMs,
      active_requests: this.pending.size,
      max_pending: this.opts.maxPending,
      frames_sent: this.counters.framesSent,
      frames_received: this.counters.framesReceived,
      malformed_frames: this.counters.malformedFrames,
      queue_overflow_responses: this.counters.queueOverflow,
      payload_too_large_rejected: this.counters.payloadTooLarge,
      timeouts_sent: this.counters.timeouts,
      runtime: this.runtimeCache,
      runtime_healthy: this.healthCache.healthy,
      runtime_health_details: this.healthCache.details ?? null,
      last_error: this.lastError,
      fatal_error: this.fatalError,
    };
  }

  /** True while the client is doing useful work (connecting/handshake/online). */
  get isRunning(): boolean {
    return !this.stopped;
  }

async #runLoop(): Promise<LinkExitInfo> {
    let first = true;
    while (true) {
      if (this.stopped) return { kind: "stopped", message: null };

      if (!first) {
        const delay = this.backoff.next();
        const plannedAttempt = this.backoff.attempt;
        if (this.opts.maxReconnectAttempts !== null && plannedAttempt > this.opts.maxReconnectAttempts) {
          return { kind: "max_reconnect", message: `exceeded ${this.opts.maxReconnectAttempts} reconnect attempts` };
        }
        this.phase = "reconnecting";
        this.reconnectAttempt = plannedAttempt;
        this.reconnectAtMs = this.now() + delay;
        this.#emit("reconnect_scheduled", {
          attempt: plannedAttempt,
          delay_ms: delay,
          reconnect_at_ms: this.reconnectAtMs,
        });
        this.#log("warn", `reconnecting in ${delay}ms (attempt ${plannedAttempt})`, { delay, attempt: plannedAttempt });
        const keepGoing = await this.#sleep(delay);
        if (!keepGoing || this.stopped) return { kind: "stopped", message: null };
      }
      first = false;

      const res = await this.#attemptConnect();
      if (this.stopped) continue;
      if (res.kind === "fatal") {
        this.fatalError = res.message;
        this.#emit("fatal", { message: res.message });
        this.#log("error", `fatal: ${res.message}`, {});
        return { kind: res.exitKind, message: res.message };
      }
    }
  }

/** Open a socket, register, then block until that socket dies. */
  async #attemptConnect(): Promise<AttemptOutcome> {
    const url = buildEdgeUrl(this.opts.edgeUrl, {
      workstationId: this.workstationId,
      linkToken: this.opts.linkToken,
    });
    let ws: LinkWebSocket;
    try {
      ws = this.opts.socketFactory(url);
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      this.lastError = `socket factory failed: ${message}`;
      this.#log("error", this.lastError, {});
      return { kind: "dropped" };
    }
    if (this.stopped) {
      this.#tryTerminate(ws);
      return { kind: "dropped" };
    }
    this.ws = ws;
    this.phase = "connecting";
    this.#emit("connecting", { attempt: this.backoff.attempt });

    let openResolve!: (ok: boolean) => void;
    let closeResolve!: (code: number, reason: string) => void;
    const openedP = new Promise<boolean>((res) => {
      openResolve = res;
    });
    const closedP = new Promise<[number, string]>((res) => {
      closeResolve = (code, reason) => res([code, reason]);
    });
    this.openResolve = openResolve;
    this.closeResolve = closeResolve;

    try {
      ws.addEventListener("open", () => {
        if (this.ws !== ws) return;
        if (this.openResolve) {
          const r = this.openResolve;
          this.openResolve = null;
          r(true);
        }
      });
      ws.addEventListener("message", (ev: any) => {
        if (this.ws !== ws) return;
        void this.#handleSocketData(ws, ev?.data).catch((err) => {
          this.lastError = `inbound frame handler threw: ${err instanceof Error ? err.message : String(err)}`;
          this.counters.malformedFrames += 1;
        });
      });
      ws.addEventListener("error", () => {
        if (this.ws !== ws) return;
        this.counters.socketErrors += 1;
      });
      ws.addEventListener("close", (ev: any) => {
        if (this.ws !== ws) return;
        const code = typeof ev?.code === "number" ? ev.code : 1006;
        const reason = typeof ev?.reason === "string" ? ev.reason : "";
        this.#handleSocketClosed(ws, code, reason);
      });
    } catch (err) {
      // A transport/socket that throws while attaching listeners must not
      // leave the open/close waiters forever pending.
      this.lastError = `socket listener attach failed: ${err instanceof Error ? err.message : String(err)}`;
      this.#log("error", this.lastError, {});
      this.openResolve?.(false);
      this.openResolve = null;
      this.closeResolve?.(-1, "");
      this.closeResolve = null;
      this.#tryTerminate(ws);
      return { kind: "dropped" };
    }

    const opened = await openedP;
    if (this.stopped) {
      this.#tryTerminate(ws);
      return { kind: "dropped" };
    }
    if (!opened) {
      this.lastError = "socket closed before opening";
      this.#log("warn", this.lastError, {});
      return { kind: "dropped" };
    }

    // Handshake: hello → await hello_ack (or structured auth refusal from
    // hello_ack.ok:false — there is no separate "error" wire frame anymore).
    this.phase = "handshake";
    this.connectionId = randomUUID();
    const hello = await this.#buildHelloMessage();
    this.#sendMessage(ws, hello);
    const outcome = await this.#awaitHandshakeAck();
    if (this.stopped) return { kind: "dropped" };
    if (!outcome.ok) {
      if (outcome.fatal) {
        const exitKind = classifyFatalCode(outcome.code);
        this.#log("error", `handshake refused: ${outcome.code} — ${outcome.message}`, {});
        this.#tryTerminate(ws);
        return { kind: "fatal", exitKind, message: outcome.message };
      }
      this.lastError = outcome.message;
      this.#tryTerminate(ws);
      return { kind: "dropped" };
    }

    // ----- online -----
    this.backoff.reset();
    this.reconnectAttempt = 0;
    this.reconnectAtMs = null;
    this.connectedAtMs = this.now();
    this.phase = "online";
    this.#emit("connected", { connected_at_ms: this.connectedAtMs });
    this.#log("info", "link online", {});
    this.#startIntervals();

    const [closeCode, closeReason] = await closedP;
    this.#disposeSocket(ws);
    this.#emit("disconnected", { code: closeCode, reason: closeReason });
    this.#log("info", "socket disconnected", { code: closeCode, reason: closeReason });
    if (closeCode === WS_CLOSE_SUPERSEDED) {
      return {
        kind: "fatal",
        exitKind: "superseded",
        message: `edge fenced this link in favor of a newer workstation connection (${closeReason})`,
      };
    }
    if (AUTH_REJECT_CLOSE_CODES.has(closeCode)) {
      return {
        kind: "fatal",
        exitKind: "auth_rejected",
        message: `edge closed link with auth refusal (${closeCode} ${closeReason})`,
      };
    }
    return { kind: "dropped" };
  }

#handleSocketClosed(ws: LinkWebSocket, code: number, reason: string): void {
    this.#disposeSocket(ws);
    if (this.openResolve) {
      const r = this.openResolve;
      this.openResolve = null;
      r(false);
    }
    if (this.closeResolve) {
      const r = this.closeResolve;
      this.closeResolve = null;
      r(code, reason);
    }
  }

  #disposeSocket(ws: LinkWebSocket): void {
    if (this.ws === ws) this.ws = null;
    this.#clearIntervals();
    this.#clearHandshakeTimer();
    if (this.forceCloseTimer) {
      clearTimeout(this.forceCloseTimer);
      this.forceCloseTimer = null;
    }
    this.lastEdgeSeenMs = null;
  }

  #sleep(ms: number): Promise<boolean> {
    if (this.stopped) return Promise.resolve(false);
    return new Promise<boolean>((resolve) => {
      const timer = setTimeout(() => {
        if (this.sleepToken) {
          this.sleepToken = null;
          resolve(true);
        }
      }, ms);
      this.sleepToken = { timer, resolve };
    });
  }

  #wakeSleep(): void {
    if (this.sleepToken) {
      const { timer, resolve } = this.sleepToken;
      this.sleepToken = null;
      clearTimeout(timer);
      resolve(false);
    }
  }

  #awaitHandshakeAck(): Promise<HandshakeOutcome> {
    return new Promise((resolve) => {
      this.handshakeResolve = resolve;
      this.handshakeTimeoutTimer = setTimeout(() => {
        if (this.handshakeResolve) {
          const r = this.handshakeResolve;
          this.handshakeResolve = null;
          this.handshakeTimeoutTimer = null;
          r({
            ok: false,
            fatal: false,
            code: "handshake_timeout",
            message: `no hello_ack within ${this.opts.handshakeTimeoutMs}ms`,
          });
        }
      }, this.opts.handshakeTimeoutMs);
    });
  }

  #clearHandshakeTimer(): void {
    if (this.handshakeTimeoutTimer) {
      clearTimeout(this.handshakeTimeoutTimer);
      this.handshakeTimeoutTimer = null;
    }
    this.handshakeResolve = null;
  }

  #tryTerminate(ws: LinkWebSocket): void {
    try {
      if (typeof ws.terminate === "function") ws.terminate();
      else if (ws.readyState === WS_OPEN || ws.readyState === WS_CONNECTING) ws.close(4000, "terminate");
    } catch {
      /* best-effort */
    }
  }

  #beginClose(ws: LinkWebSocket, code: number, reason: string): void {
    try {
      if (ws.readyState === WS_OPEN || ws.readyState === WS_CLOSING) ws.close(code, reason);
    } catch {
      this.#tryTerminate(ws);
    }
  }

  #scheduleForceClose(ms: number): void {
    this.#clearForceClose();
    this.forceCloseTimer = setTimeout(() => {
      this.forceCloseTimer = null;
      const ws = this.ws;
      if (ws) this.#tryTerminate(ws);
    }, ms);
  }

  #clearForceClose(): void {
    if (this.forceCloseTimer) {
      clearTimeout(this.forceCloseTimer);
      this.forceCloseTimer = null;
    }
  }

  #socketIsOpen(ws: LinkWebSocket): boolean {
    return ws.readyState === WS_OPEN;
  }

  #clearIntervals(): void {
    if (this.heartbeatTimer) {
      clearInterval(this.heartbeatTimer);
      this.heartbeatTimer = null;
    }
    if (this.silenceTimer) {
      clearInterval(this.silenceTimer);
      this.silenceTimer = null;
    }
  }

  #startIntervals(): void {
    this.#clearIntervals();
    this.heartbeatTimer = setInterval(() => this.#heartbeatTick(), this.opts.heartbeatMs);
    this.silenceTimer = setInterval(
      () => this.#silenceTick(),
      Math.max(250, Math.min(this.opts.heartbeatMs, this.opts.maxSilenceMs / 3)),
    );
  }

async #handleSocketData(ws: LinkWebSocket, rawData: unknown): Promise<void> {
    const raw = decodeSocketData(rawData);
    if (raw === null) {
      this.counters.malformedFrames += 1;
      return;
    }
    const bytes = Buffer.byteLength(raw, "utf8");
    if (bytes > this.opts.maxFrameBytes) {
      this.counters.payloadTooLarge += 1;
      const requestId = extractRequestId(raw);
      if (requestId) {
        this.#sendMessage(
          ws,
          encodeToolErrorMessage(
            this.workstationId,
            requestId,
            LINK_CODE.payloadTooLarge,
            false,
            "inbound frame exceeds maxFrameBytes",
            { size_bytes: bytes, max: this.opts.maxFrameBytes },
            "not_delivered",
          ),
        );
      }
      this.lastError = "oversized inbound frame dropped";
      this.#log("warn", this.lastError, { bytes, max: this.opts.maxFrameBytes });
      return;
    }
    this.counters.framesReceived += 1;
    this.lastEdgeSeenMs = this.now();

    // Canonical inbound frame validation (strict: version, kind, fields,
    // bounds). parseRelayFrame is the canonical entry point and enforces the
    // byte gate itself; we only count oversized earlier for the dedicated
    // counter + bounded error reply.
    const parsed = parseRelayFrame(raw, { maxFrameBytes: this.opts.maxFrameBytes });
    if (!parsed.ok) {
      this.counters.malformedFrames += 1;
      this.lastError = `malformed inbound frame: ${parsed.code}`;
      this.#log("warn", this.lastError, { code: parsed.code, reason: parsed.reason });
      return;
    }
    const msg = parsed.message;

    switch (msg.kind) {
      case "hello_ack":
        this.#handleHelloAckFrame(msg);
        return;
      case "tool_request":
        await this.#handleRequestFrame(ws, msg);
        return;
      case "cancel":
        this.#handleCancelFrame(ws, msg);
        return;
      case "status":
        // Canonical status(query:true) replaces the old runtime_status query.
        if (msg.query === true) await this.#pushStatusReport(ws);
        // A status report (query absent/false) from the edge is not a
        // workstation action; ignore.
        return;
      default:
        // hello/heartbeat/tool_result/tool_error/cancel_ack are
        // workstation→edge kinds; receiving them from the edge is
        // unexpected.
        this.counters.malformedFrames += 1;
    }
  }

  #handleHelloAckFrame(msg: HelloAckMessage): void {
    if (this.phase !== "handshake" || !this.handshakeResolve) return;
    if (msg.ok === true) {
      this.#resolveHandshake({
        ok: true,
        serverVersion: msg.server_version,
        edgeDeploymentId: msg.edge_deployment_id,
      });
      return;
    }
    const code = msg.code ?? "auth_rejected";
    this.#resolveHandshake({
      ok: false,
      fatal: classifyFatalCode(code) !== "fatal_error",
      code,
      message: msg.message ?? "edge refused registration",
    });
  }

  #resolveHandshake(outcome: HandshakeOutcome): void {
    const r = this.handshakeResolve;
    if (!r) return;
    this.handshakeResolve = null;
    this.#clearHandshakeTimer();
    r(outcome);
  }

  /** Canonical tool_request → internal ToolRequestFrame → runtime dispatch. */
  async #handleRequestFrame(ws: LinkWebSocket, msg: ToolRequestMessage): Promise<void> {
    if (msg.workstation_id && msg.workstation_id !== this.workstationId) return;
    if (typeof msg.request_id !== "string" || !msg.request_id) {
      this.counters.malformedFrames += 1;
      return;
    }
    if (typeof msg.operation !== "string" || !msg.operation) {
      this.#sendMessage(
        ws,
        encodeToolErrorMessage(this.workstationId, msg.request_id, "bad_request", false, "operation string is required", undefined, "not_delivered"),
      );
      return;
    }
    if (this.stopped) {
      this.#sendMessage(
        ws,
        encodeToolErrorMessage(this.workstationId, msg.request_id, LINK_CODE.linkStopping, true, "link is shutting down", undefined, "not_delivered"),
      );
      return;
    }
    const req = toInternalRequest(msg);
    if (this.pending.has(req.request_id)) {
      this.#sendMessage(
        ws,
        encodeToolErrorMessage(this.workstationId, req.request_id, LINK_CODE.duplicateRequest, true, "request already in flight", undefined, "delivery_unknown"),
      );
      return;
    }
    if (this.pending.size >= this.opts.maxPending) {
      this.counters.queueOverflow += 1;
      this.#sendMessage(
        ws,
        encodeToolErrorMessage(
          this.workstationId,
          req.request_id,
          LINK_CODE.queueFull,
          true,
          `pending queue full (max ${this.opts.maxPending})`,
          undefined,
          "not_delivered",
        ),
      );
      return;
    }

    const timeoutMs = clampRequestTimeout(req.timeout_ms, this.opts.requestTimeoutMs);
    const slot = new PendingSlot(req, timeoutMs, this.now());
    this.pending.set(req.request_id, slot);
    this.#emit("request_started", { request_id: req.request_id });
    slot.timer = setTimeout(() => this.#handleTimeout(slot), timeoutMs);
    void this.#dispatchAndSettle(slot);
  }

  /** Run the injected transport and send the correlated canonical result. */
  async #dispatchAndSettle(slot: PendingSlot): Promise<void> {
    let result: RuntimeToolResult;
    const t0 = this.now();
    try {
      result = await this.transport.dispatchRequest(slot.req);
    } catch (err) {
      result = {
        ok: false,
        code: LINK_CODE.transportError,
        retryable: true,
        error: { message: err instanceof Error ? err.message : String(err) },
      };
    }
    const runtimeMs = this.now() - t0;
    if (!slot.claimSettle()) {
      this.#log("debug", "late transport result dropped (already settled)", { request_id: slot.req.request_id });
      return;
    }
    this.#dropPending(slot);
    const ws = this.ws;
    if (ws) this.#sendResultResponse(ws, slot.req, result, runtimeMs);
    const final = result.ok ? "ok" : "error";
    this.#emit("request_finished", {
      request_id: slot.req.request_id,
      final_status: final,
      code: result.ok ? "ok" : result.code,
      retryable: result.ok ? true : result.retryable,
    });
  }

  #handleTimeout(slot: PendingSlot): void {
    if (slot.timer) {
      clearTimeout(slot.timer);
      slot.timer = null;
    }
    if (!slot.claimSettle()) return;
    this.counters.timeouts += 1;
    this.#dropPending(slot);
    const ws = this.ws;
    if (ws) {
      this.#sendMessage(
        ws,
        encodeToolErrorMessage(
          this.workstationId,
          slot.req.request_id,
          LINK_CODE.requestTimeout,
          true,
          `request exceeded ${slot.timeoutMs}ms local budget; execution state unknown`,
          { timeout_ms: slot.timeoutMs },
          "delivery_unknown",
        ),
      );
    }
    this.#emit("request_finished", {
      request_id: slot.req.request_id,
      final_status: "timeout",
      code: LINK_CODE.requestTimeout,
      retryable: true,
    });
  }

  /**
   * Canonical inbound cancel → settle slot + notify runtime + return
   * cancel_ack. Unknown/already-settled requests get cancel_ack(accepted:false).
   */
  #handleCancelFrame(ws: LinkWebSocket, msg: CancelMessage): void {
    if (msg.workstation_id && msg.workstation_id !== this.workstationId) return;
    const slot = this.pending.get(msg.request_id);
    if (!slot) {
      this.#sendMessage(
        ws,
        encodeCancelAckMessage(this.workstationId, msg.request_id, false, this.now(), "no in-flight request"),
      );
      return;
    }
    if (!slot.claimSettle()) {
      this.#sendMessage(
        ws,
        encodeCancelAckMessage(this.workstationId, msg.request_id, false, this.now(), "already settled"),
      );
      return;
    }
    slot.cancelled = true;
    if (slot.timer) {
      clearTimeout(slot.timer);
      slot.timer = null;
    }
    this.pending.delete(msg.request_id);
    this.#sendMessage(
      ws,
      encodeCancelAckMessage(this.workstationId, msg.request_id, true, this.now(), msg.reason ?? null),
    );
    this.#emit("request_finished", {
      request_id: msg.request_id,
      final_status: "cancelled",
      code: LINK_CODE.cancelled,
      retryable: false,
    });
    this.#log("info", "cancel dispatched to runtime", { request_id: msg.request_id });
    void this.transport
      .cancelRequest(msg.request_id, msg.reason ?? "edge_cancel")
      .catch((err) => this.#log("warn", "transport cancelRequest failed", err));
  }

  #dropPending(slot: PendingSlot): void {
    if (this.pending.get(slot.req.request_id) === slot) {
      this.pending.delete(slot.req.request_id);
    }
  }

  /** After a hard stop, settle every leftover locally (edge will re-route). */
  #rejectAllPending(code: string, message: string, retryable: boolean): void {
    for (const slot of Array.from(this.pending.values())) {
      if (!slot.claimSettle()) continue;
      if (slot.timer) {
        clearTimeout(slot.timer);
        slot.timer = null;
      }
      slot.cancelled = true;
      const ws = this.ws;
      if (ws) {
        this.#sendMessage(
          ws,
          encodeToolErrorMessage(this.workstationId, slot.req.request_id, code, retryable, message, undefined, "not_delivered"),
        );
      }
    }
    this.pending.clear();
  }

  async #waitForPendingEmpty(ms: number): Promise<void> {
    if (this.pending.size === 0) return;
    const deadline = this.now() + ms;
    while (this.pending.size > 0) {
      const remaining = deadline - this.now();
      if (remaining <= 0) break;
      await this.#delay(Math.min(50, remaining));
    }
  }

  #delay(ms: number): Promise<void> {
    return new Promise((resolve) => setTimeout(resolve, ms));
  }

  /** Canonical outbound result: tool_result on ok, tool_error on failure. */
  #sendResultResponse(
    ws: LinkWebSocket,
    req: ToolRequestFrame,
    result: RuntimeToolResult,
    runtimeMs: number,
  ): void {
    const servedAtMs = this.now();
    const runtimeGeneration = this.runtimeCache?.runtime_generation ?? null;
    if (result.ok) {
      this.#sendMessage(
        ws,
        encodeToolResultMessage(
          this.workstationId,
          req.request_id,
          (result as { result: unknown }).result,
          servedAtMs,
          runtimeGeneration,
          this.transport.name,
        ),
      );
      return;
    }
    const err = result as { code: string; retryable: boolean; error: { message: string; details?: unknown } };
    this.#sendMessage(
      ws,
      encodeToolErrorMessage(
        this.workstationId,
        req.request_id,
        err.code,
        err.retryable,
        err.error?.message,
        err.error?.details,
        "delivered",
        servedAtMs,
        runtimeGeneration,
      ),
    );
    void runtimeMs; // timing is internal; canonical wire has no runtime_ms field
  }

  /** Serialize a canonical message, enforce the payload budget, send. */
  #sendMessage(ws: LinkWebSocket, msg: RelayMessage): void {
    if (!ws || ws !== this.ws) return;
    let raw: string;
    try {
      raw = JSON.stringify(msg);
    } catch {
      this.counters.malformedFrames += 1;
      return;
    }
    if (raw === undefined) {
      this.counters.malformedFrames += 1;
      return;
    }
    const bytes = Buffer.byteLength(raw, "utf8");
    if (bytes > this.opts.maxFrameBytes) {
      this.counters.payloadTooLarge += 1;
      if (msg.kind === "tool_result") {
        // Directly emit a compact bounded canonical tool_error so the edge
        // still gets correlation.
        this.#sendCompactOversizedError(ws, (msg as ToolResultMessage).request_id);
      } else {
        this.#log("warn", "outbound frame dropped (oversized)", { kind: msg.kind, bytes, max: this.opts.maxFrameBytes });
      }
      return;
    }
    try {
      ws.send(raw);
      this.counters.framesSent += 1;
    } catch (err) {
      this.lastError = err instanceof Error ? `send failed: ${err.message}` : "send failed";
      this.counters.socketErrors += 1;
    }
  }

  /** Never re-enter #sendMessage; emit the smallest canonical error frame. */
  #sendCompactOversizedError(ws: LinkWebSocket, requestId: string): void {
    const compact: ToolErrorMessage = encodeCompactOversizedError(
      this.workstationId,
      requestId,
      this.runtimeCache?.runtime_generation ?? null,
    );
    let raw: string;
    try {
      raw = JSON.stringify(compact);
    } catch {
      return;
    }
    if (Buffer.byteLength(raw, "utf8") > this.opts.maxFrameBytes) return; // even compact does not fit → drop
    try {
      ws.send(raw);
      this.counters.framesSent += 1;
    } catch {
      /* socket is going down anyway */
    }
  }

async #heartbeatTick(): Promise<void> {
    if (this.phase !== "online" || this.stopped || !this.ws) return;
    const ws = this.ws;
    if (!this.#socketIsOpen(ws)) return;
    let runtime: RuntimeIdentitySnapshot;
    try {
      runtime = await this.#runtimeSnapshot();
    } catch {
      runtime = this.runtimeCache ?? EMPTY_RUNTIME;
    }
    this.lastHeartbeatMs = this.now();
    const msg: HeartbeatMessage = encodeHeartbeatMessage(
      this.workstationId,
      this.bootId,
      this.pending.size,
      runtime,
      this.now() - this.startedAt,
      this.now(),
    );
    this.#sendMessage(ws, msg);
    this.#emit("heartbeat_sent", { at_ms: this.lastHeartbeatMs });
  }

  #silenceTick(): void {
    if (this.phase !== "online" || this.stopped || !this.ws) return;
    const base = this.lastEdgeSeenMs ?? this.connectedAtMs ?? 0;
    if (this.now() - base > this.opts.maxSilenceMs) {
      this.lastError = `no frames from edge for ${this.opts.maxSilenceMs}ms; recycling socket`;
      this.#log("warn", this.lastError, {});
      this.#clearIntervals();
      this.#tryTerminate(this.ws);
    }
  }

  async #buildHelloMessage(): Promise<RelayMessage> {
    const runtime = await this.#runtimeSnapshot(true);
    return encodeHelloMessage(
      this.workstationId,
      this.bootId,
      this.opts.linkVersion,
      [...DEFAULT_CAPABILITIES],
      runtime,
      this.now(),
    );
  }

  async #runtimeSnapshot(fresh = false): Promise<RuntimeIdentitySnapshot> {
    if (!fresh && this.runtimeCache && this.now() - this.runtimeCacheAtMs < RUNTIME_CACHE_TTL_MS) {
      return this.runtimeCache;
    }
    try {
      const s = await this.transport.getRuntimeInfo();
      this.runtimeCache = s;
      this.runtimeCacheAtMs = this.now();
      return s;
    } catch (err) {
      this.lastError = `runtime info unavailable: ${err instanceof Error ? err.message : String(err)}`;
      this.#log("warn", this.lastError, {});
      if (this.runtimeCache) return this.runtimeCache;
      return EMPTY_RUNTIME;
    }
  }

  /** Canonical status report in reply to inbound status(query:true). */
  async #pushStatusReport(ws: LinkWebSocket): Promise<void> {
    const runtime = await this.#runtimeSnapshot(true);
    let health: { healthy: boolean; details?: string } = { healthy: true };
    try {
      health = await this.transport.getHealth();
      this.healthCache = health;
    } catch {
      /* health is advisory only */
    }
    const msg: StatusMessage = encodeStatusReport(
      this.workstationId,
      runtime,
      health.healthy,
      health.details ?? null,
      this.pending.size,
      this.now() - this.startedAt,
      this.lastError,
      this.now(),
    );
    this.#sendMessage(ws, msg);
  }

#emit<K extends keyof LinkEventMap>(type: K, payload: LinkEventMap[K]): void {
    if (!this.onEvent) return;
    try {
      this.onEvent({ type, ...(payload as object) } as LinkEvent);
    } catch (err) {
      this.#log("error", "onEvent callback threw", err);
    }
  }

  #log(level: "debug" | "info" | "warn" | "error", msg: string, extra: unknown): void {
    const fn = this.opts.logger?.[level];
    if (typeof fn !== "function") return;
    try {
      fn(`[herdr-link:${this.workstationId}] ${msg}`, extra);
    } catch {
      /* logging must never crash the link */
    }
  }

  /** Per-attempt resolver that the inbound hello_ack fills. */
  private handshakeResolve: ((outcome: HandshakeOutcome) => void) | null = null;
}

// ─────────────────────────────────────────────────────────────────────────────
// Module-level helpers (pure, unit-testable).
// ─────────────────────────────────────────────────────────────────────────────

/** Fallback runtime identity when the transport cannot answer yet. */
export const EMPTY_RUNTIME: RuntimeIdentitySnapshot = {
  runtime_version: "unknown",
  runtime_commit: null,
  runtime_generation: null,
  contract_epoch: 0,
  contract_hash: null,
  herdr_version: null,
  herdr_protocol: null,
};

/** Clamp a numeric option into [min, max], falling back only for non-numbers. */
export function clampRange(value: number | undefined, fallback: number, min: number, max: number): number {
  if (value === undefined || !Number.isFinite(value)) return fallback;
  return Math.min(max, Math.max(min, Math.floor(value)));
}

/** Request-local deadline budget: use the hint but never exceed the cap. */
export function clampRequestTimeout(hint: number | undefined, max: number): number {
  if (hint === undefined || !Number.isFinite(hint) || hint < 1) return max;
  return Math.max(1, Math.min(Math.floor(hint), max));
}

/** Strip credentials before the displayed/status URL. */
export function redactUrl(url: string): string {
  try {
    const u = new URL(url);
    if (u.searchParams.has("link_token")) u.searchParams.set("link_token", "***");
    return u.toString();
  } catch {
    return url.replace(/link_token=[^&]*/g, "link_token=***");
  }
}

/**
 * Map a refused-hello code to exit kind (auth vs contract vs other).
 */
export function classifyFatalCode(code: string): "auth_rejected" | "contract_rejected" | "fatal_error" {
  if (code === "auth_rejected" || code === "auth_expired" || code === "session_invalid") return "auth_rejected";
  if (code === "contract_mismatch" || code === "contract_rejected" || code === "protocol_incompatible") return "contract_rejected";
  return "fatal_error";
}

/** Coerce raw socket data (string | Buffer | ArrayBuffer) to text or null. */
export function decodeSocketData(data: unknown): string | null {
  if (typeof data === "string") return data;
  if (data instanceof ArrayBuffer) return new TextDecoder().decode(data);
  if (ArrayBuffer.isView(data)) return new TextDecoder().decode(new Uint8Array(data.buffer, data.byteOffset, data.byteLength));
  if (data == null) return null;
  return String(data);
}

/** Cheap request_id extraction for frames that exceed the frame budget. */
export function extractRequestId(raw: string): string | null {
  try {
    const obj = JSON.parse(raw) as { request_id?: unknown };
    if (obj && typeof obj === "object" && typeof obj.request_id === "string" && obj.request_id) return obj.request_id;
    return null;
  } catch {
    return null;
  }
}