/**
 * local-mcp-transport.ts — LocalMcpRuntimeTransport (herdr-link Phase 2).
 *
 * The workstation-side herdr-link invokes the LOCAL Herdr runtime through the
 * MCP Streamable HTTP `tools/call` method. This transport is stateless: every
 * dispatch is one independent JSON-RPC POST (no Mcp-Session-Id, no
 * initialize). It does NOT inspect Herdr internals and never reads plist /
 * token files — identity (contract hash, runtime version) is configured by
 * the caller, and every network call goes through an injectable `fetch` so it
 * can be driven by mocks in unit tests without touching the live runtime.
 *
 * Wire contract per dispatch:
 *
 *   POST <endpoint>
 *   Authorization: Bearer <token>          (never logged / returned)
 *   Content-Type: application/json
 *   Accept: application/json, text/event-stream
 *
 *   {"jsonrpc":"2.0","id":"<local id>","method":"tools/call",
 *    "params":{"name":<operation>,"arguments":<arguments ?? {}>}}
 *
 * The reply may be plain JSON or an SSE `data:` stream (comments/blank lines
 * ignored, multi-line `data:` joined with "\n"). Replies are ONLY accepted
 * when the JSON-RPC `id` exactly matches the id we sent. A successful reply
 * is returned in full as `{ ok:true, result }`, where `result` is the COMPLETE
 * MCP CallToolResult envelope — including image blocks and `isError:true` —
 * so nothing is lost on the relay edge passthrough. `isError:true` stays
 * inside the successful envelope; it is NOT converted into a transport
 * failure.
 *
 * Security rules enforced here:
 *  - the bearer token never appears in thrown errors, returned error details,
 *    health details or logs;
 *  - request arguments and raw response bodies never appear in errors;
 *  - JSON-RPC error messages are not forwarded (they can echo arguments);
 *  - response bodies are read incrementally and bounded by maxFrameBytes;
 *  - loopback-only endpoint guard unless `allowNonLoopback` is explicit.
 */

import type {
  LinkRuntimeTransport,
  RequestId,
  RuntimeIdentitySnapshot,
  RuntimeToolResult,
  ToolRequestFrame,
} from "./types.js";

/** Default local runtime MCP endpoint (live runtime runs on 127.0.0.1:8772). */
export const LOCAL_MCP_DEFAULT_ENDPOINT = "http://127.0.0.1:8772/mcp";
/** Default bound on serialized request and read response bodies (2 MiB). */
export const LOCAL_MCP_DEFAULT_MAX_FRAME_BYTES = 2 * 1024 * 1024;
/** Default per-request budget when the frame carries no timeout hint. */
export const LOCAL_MCP_DEFAULT_TIMEOUT_MS = 10_000;
/** Hard cap applied on top of any request timeout hint. */
export const LOCAL_MCP_MAX_TIMEOUT_MS = 120_000;
/** Current public contract epoch advertised by this transport. */
export const LOCAL_MCP_CONTRACT_EPOCH = 2 as const;

/** Stable failure codes emitted by this transport (RuntimeToolResult.code). */
export const LOCAL_MCP_CODE = {
  badRequest: "local_mcp_bad_request",
  duplicateRequest: "local_mcp_duplicate_request",
  requestTooLarge: "local_mcp_request_too_large",
  timeout: "local_mcp_timeout",
  cancelled: "local_mcp_cancelled",
  unreachable: "local_mcp_unreachable",
  httpError: "local_mcp_http_error",
  responseTooLarge: "local_mcp_response_too_large",
  malformedResponse: "local_mcp_malformed_response",
  idMismatch: "local_mcp_id_mismatch",
  jsonrpcError: "local_mcp_jsonrpc_error",
} as const;

/** Common locations where an MCP result may carry the server version. */
const VERSION_FIELDS = ["serverInfo.version", "server_info.version", "version"] as const;

export interface LocalMcpRuntimeTransportOptions {
  /** Local runtime MCP endpoint (default http://127.0.0.1:8772/mcp). */
  endpoint?: string | URL;
  /** Required. Sent as `Authorization: Bearer <token>`; never returned. */
  bearerToken: string;
  /** Required. Contract hash the link advertises in runtime identity. */
  contractHash: string;
  /** Contract epoch. This build serves the frozen public epoch-2 contract. */
  contractEpoch?: number;
  /** Optional configured runtime version (discovery can add a fallback). */
  runtimeVersion?: string;
  runtimeCommit?: string | null;
  runtimeGeneration?: string | null;
  herdrVersion?: string | null;
  herdrProtocol?: string | null;
  /** Injectable fetch (required in Node < 18 tests / mocks). */
  fetch?: typeof globalThis.fetch;
  /** Allow non-loopback endpoints (TEST ONLY — never in production config). */
  allowNonLoopback?: boolean;
  /** Per-request budget when the frame has no timeout_ms (default 10_000). */
  defaultTimeoutMs?: number;
  /** Hard cap applied on any request timeout before dispatch (default 120_000). */
  maxTimeoutMs?: number;
  /** Bound on request body and response body size (default 2 MiB). */
  maxFrameBytes?: number;
  /** Health probe override (defaults to sessionless `server/discover`). */
  healthProbe?: {
    method?: string;
    params?: Record<string, unknown>;
  };
}

/** One in-flight dispatch's abort state keyed by relay request_id. */
interface AbortEntry {
  controller: AbortController;
  /** Why the signal is being aborted (null until timeout/cancel fires). */
  reason: "timeout" | "cancel" | null;
}

/** Result of parsing a bounded MCP response body. */
type ParsedBody =
  | { kind: "result"; result: unknown }
  | { kind: "rpc_error"; error: { code?: unknown } }
  | { kind: "id_mismatch"; parsed: number }
  | { kind: "malformed" };

/**
 * Stateless local MCP HTTP transport implementing `LinkRuntimeTransport`.
 *
 * Identity: contract_epoch is frozen at the public epoch and contract_hash is caller-supplied;
 * runtime_version may be configured and is supplemented by a version cached
 * from the optional health/discover probe.
 */
export class LocalMcpRuntimeTransport implements LinkRuntimeTransport {
  readonly name = "local-mcp-http";

  private readonly endpoint: URL;
  private readonly bearerToken: string;
  private readonly contractHash: string;
  private readonly fetchFn: typeof globalThis.fetch;
  private readonly defaultTimeoutMs: number;
  private readonly maxTimeoutMs: number;
  private readonly maxFrameBytes: number;
  private readonly healthMethod: string;
  private readonly healthParams: Record<string, unknown>;

  private readonly runtimeVersion: string | null;
  private readonly runtimeCommit: string | null;
  private readonly runtimeGeneration: string | null;
  private readonly herdrVersion: string | null;
  private readonly herdrProtocol: string | null;

  /** Version discovered from a healthy server/discover probe (cached). */
  private discoveredVersion: string | null = null;

  /** request_id -> abort entry for currently in-flight dispatches. */
  private readonly inFlight = new Map<string, AbortEntry>();

  private idCounter = 0;

  constructor(options: LocalMcpRuntimeTransportOptions) {
    const token = options.bearerToken;
    if (typeof token !== "string" || token.length === 0) {
      throw new TypeError("local-mcp: bearerToken (non-empty string) is required");
    }
    const hash = options.contractHash;
    if (typeof hash !== "string" || hash.length === 0) {
      throw new TypeError("local-mcp: contractHash (non-empty string) is required");
    }
    const epoch = options.contractEpoch ?? LOCAL_MCP_CONTRACT_EPOCH;
    if (epoch !== LOCAL_MCP_CONTRACT_EPOCH) {
      // Intentionally does not echo caller values; the public epoch is frozen.
      throw new RangeError(`local-mcp: contractEpoch must be ${LOCAL_MCP_CONTRACT_EPOCH} (public contract epoch)`);
    }

    let url: URL;
    try {
      url = new URL(String(options.endpoint ?? LOCAL_MCP_DEFAULT_ENDPOINT));
    } catch {
      throw new TypeError("local-mcp: endpoint must be a valid URL");
    }
    if (url.protocol !== "http:" && url.protocol !== "https:") {
      throw new TypeError("local-mcp: endpoint must use http(s)");
    }
    if (!(options.allowNonLoopback === true) && !isLoopbackHost(url.hostname)) {
      throw new TypeError(
        "local-mcp: endpoint must be a loopback URL (127.0.0.0/8, ::1, localhost) unless allowNonLoopback is set",
      );
    }

    const fetchFn = options.fetch ?? globalThis.fetch;
    if (typeof fetchFn !== "function") {
      throw new TypeError("local-mcp: fetch implementation is required");
    }

    const probe = options.healthProbe ?? {};
    const healthMethod =
      typeof probe.method === "string" && probe.method.length > 0 ? probe.method : "server/discover";
    const healthParams =
      probe.params !== undefined && probe.params !== null && typeof probe.params === "object"
        ? probe.params
        : {};

    this.endpoint = url;
    this.bearerToken = token;
    this.contractHash = hash;
    this.fetchFn = fetchFn;
    this.defaultTimeoutMs = clampPositive(options.defaultTimeoutMs, LOCAL_MCP_DEFAULT_TIMEOUT_MS);
    this.maxTimeoutMs = clampPositive(options.maxTimeoutMs, LOCAL_MCP_MAX_TIMEOUT_MS);
    this.maxFrameBytes = clampPositive(
      options.maxFrameBytes,
      LOCAL_MCP_DEFAULT_MAX_FRAME_BYTES,
      64,
      64 * 1024 * 1024,
    );
    this.healthMethod = healthMethod;
    this.healthParams = healthParams;

    this.runtimeVersion =
      typeof options.runtimeVersion === "string" && options.runtimeVersion.length > 0
        ? options.runtimeVersion
        : null;
    this.runtimeCommit = options.runtimeCommit ?? null;
    this.runtimeGeneration = options.runtimeGeneration ?? null;
    this.herdrVersion = options.herdrVersion ?? null;
    this.herdrProtocol = options.herdrProtocol ?? null;
  }

  /** Cheap, network-free identity snapshot: configured + discovered fields. */
  getRuntimeInfo(): RuntimeIdentitySnapshot {
    return {
      runtime_version: this.runtimeVersion ?? this.discoveredVersion ?? "unknown",
      runtime_commit: this.runtimeCommit,
      runtime_generation: this.runtimeGeneration,
      contract_epoch: LOCAL_MCP_CONTRACT_EPOCH,
      contract_hash: this.contractHash,
      herdr_version: this.herdrVersion,
      herdr_protocol: this.herdrProtocol,
    };
  }

  /**
   * Dispatch one relay tool request as a stateless MCP `tools/call` POST.
   * Always resolves (never throws): failures are sanitized RuntimeToolResult
   * values. request_id drives cancellation correlation; a separate JSON-RPC
   * id correlates the HTTP reply.
   */
  async dispatchRequest(req: ToolRequestFrame): Promise<RuntimeToolResult> {
    const requestId = req?.request_id;
    if (typeof requestId !== "string" || requestId.length === 0) {
      return this.failure(LOCAL_MCP_CODE.badRequest, false, "invalid tool request frame");
    }
    const operation = req.operation;
    if (typeof operation !== "string" || operation.length === 0) {
      return this.failure(LOCAL_MCP_CODE.badRequest, false, "tool request is missing an operation");
    }
    if (this.inFlight.has(requestId)) {
      return this.failure(LOCAL_MCP_CODE.duplicateRequest, false, "request_id is already in flight");
    }

    const rpcId = this.nextId();
    const body = JSON.stringify({
      jsonrpc: "2.0",
      id: rpcId,
      method: "tools/call",
      params: {
        name: operation,
        arguments: req.arguments ?? {},
      },
    });
    if (utf8ByteLength(body) > this.maxFrameBytes) {
      return this.failure(LOCAL_MCP_CODE.requestTooLarge, false, "request exceeds maxFrameBytes", {
        max_bytes: this.maxFrameBytes,
      });
    }

    const timeoutMs = clampRequestTimeout(req.timeout_ms, this.defaultTimeoutMs, this.maxTimeoutMs);
    const controller = new AbortController();
    const entry: AbortEntry = { controller, reason: null };
    this.inFlight.set(requestId, entry);
    const timer = setTimeout(() => {
      // Timeout wins over a later cancel once it has fired.
      if (entry.reason === null) entry.reason = "timeout";
      controller.abort();
    }, timeoutMs);

    try {
      let res: Response;
      try {
        res = await this.fetchFn(String(this.endpoint), {
          method: "POST",
          headers: {
            Authorization: `Bearer ${this.bearerToken}`,
            "Content-Type": "application/json",
            Accept: "application/json, text/event-stream",
          },
          body,
          signal: controller.signal,
        });
      } catch {
        if (controller.signal.aborted) {
          return entry.reason === "timeout"
            ? this.failure(LOCAL_MCP_CODE.timeout, true, "local runtime request timed out", {
                timeout_ms: timeoutMs,
              })
            : this.failure(LOCAL_MCP_CODE.cancelled, false, "local runtime request cancelled");
        }
        return this.failure(LOCAL_MCP_CODE.unreachable, true, "local runtime unreachable");
      }

      let limited;
      try {
        limited = await this.readBoundedBody(res, controller);
      } catch {
        if (controller.signal.aborted) {
          return entry.reason === "timeout"
            ? this.failure(LOCAL_MCP_CODE.timeout, true, "local runtime request timed out", {
                timeout_ms: timeoutMs,
              })
            : this.failure(LOCAL_MCP_CODE.cancelled, false, "local runtime request cancelled");
        }
        return this.failure(LOCAL_MCP_CODE.malformedResponse, false, "malformed MCP response");
      }
      if (!limited.ok) {
        return this.failure(LOCAL_MCP_CODE.responseTooLarge, false, "response exceeds maxFrameBytes", {
          max_bytes: this.maxFrameBytes,
        });
      }

      const parsed = parseMcpBody(limited.text, rpcId);
      if (parsed.kind === "rpc_error") {
        // Never forward the remote error message or error.data — they can
        // echo request arguments/secrets. Only the stable numeric code.
        return this.failure(LOCAL_MCP_CODE.jsonrpcError, false, "local runtime reported a JSON-RPC error", {
          rpc_code: typeof parsed.error.code === "number" ? parsed.error.code : null,
        });
      }
      if (!res.ok) {
        // Transport-level status failure (JSON-RPC errors were already
        // honored above). 429/5xx are safe to retry; other 4xx are not.
        const retryable = res.status === 429 || res.status >= 500;
        return this.failure(LOCAL_MCP_CODE.httpError, retryable, "local runtime returned an HTTP error", {
          status: res.status,
        });
      }
      if (parsed.kind === "malformed") {
        return this.failure(LOCAL_MCP_CODE.malformedResponse, false, "malformed MCP response");
      }
      if (parsed.kind === "id_mismatch") {
        return this.failure(LOCAL_MCP_CODE.idMismatch, false, "MCP response id does not match request");
      }
      // Full MCP CallToolResult envelope preserved verbatim (image blocks and
      // isError:true ride inside a successful result on purpose).
      return { ok: true, result: parsed.result };
    } finally {
      clearTimeout(timer);
      // Only remove our own entry; a newer dispatch with the same id after a
      // completed/cancelled request must not be clobbered by this cleanup.
      if (this.inFlight.get(requestId) === entry) this.inFlight.delete(requestId);
    }
  }

  /**
   * Abort only the in-flight fetch matching request_id. Idempotent and
   * harmless for unknown/already-settled requests.
   */
  async cancelRequest(requestId: RequestId, reason: string): Promise<void> {
    const entry = this.inFlight.get(requestId);
    if (!entry) return; // unknown or already settled — no-op
    // Honor a timeout that already fired; otherwise record the explicit cancel.
    entry.reason = entry.reason ?? "cancel";
    void reason; // the reason text is internal; never surfaced in errors
    try {
      entry.controller.abort();
    } catch {
      /* abort on an already-aborted controller is a no-op */
    }
  }

  /**
   * Cheap sessionless health probe (default server/discover, never a tool).
   * Failure details are a short sanitized category — never token/body/args.
   */
  async getHealth(): Promise<{ healthy: boolean; details?: string }> {
    const rpcId = this.nextId();
    const body = JSON.stringify({
      jsonrpc: "2.0",
      id: rpcId,
      method: this.healthMethod,
      params: this.healthParams,
    });
    const controller = new AbortController();
    const timeoutMs = clampRequestTimeout(undefined, this.defaultTimeoutMs, this.maxTimeoutMs);
    const timer = setTimeout(() => controller.abort(), timeoutMs);

    try {
      let res: Response;
      try {
        res = await this.fetchFn(String(this.endpoint), {
          method: "POST",
          headers: {
            Authorization: `Bearer ${this.bearerToken}`,
            "Content-Type": "application/json",
            Accept: "application/json, text/event-stream",
          },
          body,
          signal: controller.signal,
        });
      } catch {
        return { healthy: false, details: controller.signal.aborted ? "timeout" : "unreachable" };
      }
      if (!res.ok) {
        return { healthy: false, details: `http_${res.status}` };
      }
      let limited;
      try {
        limited = await this.readBoundedBody(res, controller);
      } catch {
        return { healthy: false, details: controller.signal.aborted ? "timeout" : "malformed" };
      }
      if (!limited.ok) return { healthy: false, details: "response_too_large" };
      const parsed = parseMcpBody(limited.text, rpcId);
      if (parsed.kind === "malformed") return { healthy: false, details: "malformed" };
      if (parsed.kind === "id_mismatch") return { healthy: false, details: "id_mismatch" };
      if (parsed.kind === "rpc_error") return { healthy: false, details: "rpc_error" };
      this.cacheServerVersion(parsed.result);
      return { healthy: true };
    } finally {
      clearTimeout(timer);
    }
  }

  /** Read a response body incrementally, bounding raw bytes before decode. */
  private async readBoundedBody(
    res: Response,
    controller: AbortController,
  ): Promise<{ ok: true; text: string } | { ok: false }> {
    const body = res.body;
    if (!body) {
      // Status-only responses: nothing to bound.
      let text = "";
      try {
        text = await res.text();
      } catch {
        /* fall through with empty text */
      }
      return { ok: true, text };
    }

    let reader: ReadableStreamDefaultReader<Uint8Array>;
    try {
      reader = body.getReader();
    } catch {
      // Body already locked (should not happen with our own fetch); fall back
      // to text() with a post-hoc byte check.
      let text = "";
      try {
        text = await res.text();
      } catch {
        /* ignore */
      }
      if (utf8ByteLength(text) > this.maxFrameBytes) return { ok: false };
      return { ok: true, text };
    }

    const chunks: Uint8Array[] = [];
    let total = 0;
    try {
      for (;;) {
        const step = await reader.read();
        if (step.done) break;
        const value = step.value;
        if (!value || value.byteLength === 0) continue;
        total += value.byteLength;
        if (total > this.maxFrameBytes) {
          try {
            void reader.cancel();
          } catch {
            /* best-effort stop of the download */
          }
          return { ok: false };
        }
        chunks.push(value);
      }
    } catch (err) {
      // An abort mid-stream must surface as timeout/cancelled (the caller
      // inspects controller.signal.aborted), never as malformed content.
      if (controller.signal.aborted) throw err;
      throw new Error("local-mcp: response body read failed");
    }
    try {
      reader.releaseLock();
    } catch {
      /* ignore */
    }
    const text = new TextDecoder().decode(concatBytes(chunks));
    return { ok: true, text };
  }

  /** Cache a server version found in a healthy probe result (best-effort). */
  private cacheServerVersion(result: unknown): void {
    if (result === null || typeof result !== "object" || Array.isArray(result)) return;
    const obj = result as Record<string, unknown>;
    const meta = obj._meta;
    if (meta !== null && typeof meta === "object" && !Array.isArray(meta)) {
      const serverInfo = (meta as Record<string, unknown>)["io.modelcontextprotocol/serverInfo"];
      if (serverInfo !== null && typeof serverInfo === "object" && !Array.isArray(serverInfo)) {
        const version = (serverInfo as Record<string, unknown>).version;
        if (typeof version === "string" && version.length > 0) {
          this.discoveredVersion = version;
          return;
        }
      }
    }
    for (const field of VERSION_FIELDS) {
      const parts = field.split(".");
      let cursor: unknown = obj;
      for (const part of parts) {
        if (cursor === null || typeof cursor !== "object") {
          cursor = undefined;
          break;
        }
        cursor = (cursor as Record<string, unknown>)[part];
      }
      if (typeof cursor === "string" && cursor.length > 0) {
        this.discoveredVersion = cursor;
        return;
      }
    }
  }

  /** Fresh JSON-RPC id, unique within this transport instance. */
  private nextId(): string {
    this.idCounter += 1;
    return `local-${this.idCounter}`;
  }

  /** Build a sanitized failed RuntimeToolResult; never includes secret data. */
  private failure(
    code: string,
    retryable: boolean,
    message: string,
    details?: Record<string, unknown>,
  ): RuntimeToolResult {
    return {
      ok: false,
      code,
      retryable,
      error: details === undefined ? { message } : { message, details },
    };
  }
}

// ─────────────────────────────────────────────────────────────────────────────
// Pure helpers (unit-testable).
// ─────────────────────────────────────────────────────────────────────────────

/** True for 127.0.0.0/8, ::1 and localhost (normalized IPv6 handled). */
export function isLoopbackHost(hostname: string): boolean {
  let h = hostname;
  if (h.startsWith("[") && h.endsWith("]")) h = h.slice(1, -1);
  const lower = h.toLowerCase();
  if (lower === "localhost") return true;
  if (/^127\.\d{1,3}\.\d{1,3}\.\d{1,3}$/.test(lower)) {
    return lower.split(".").every((octet) => {
      if (!/^\d{1,3}$/.test(octet)) return false;
      return Number(octet) >= 0 && Number(octet) <= 255;
    });
  }
  return lower === "::1" || lower === "0:0:0:0:0:0:0:1" || lower === "0:0:0:0:0:0:0:0:1";
}

/** Clamp a finite positive integer option; fallback for missing/bad values. */
function clampPositive(value: number | undefined, fallback: number, min = 1, max = Number.MAX_SAFE_INTEGER): number {
  if (value === undefined || !Number.isFinite(value)) return fallback;
  return Math.min(max, Math.max(min, Math.floor(value)));
}

/**
 * Effective request timeout: the frame hint (if present/positive) capped by
 * maxTimeoutMs; otherwise the transport default capped the same way.
 */
function clampRequestTimeout(hint: number | undefined, fallback: number, max: number): number {
  const base = hint !== undefined && Number.isFinite(hint) && hint >= 1 ? hint : fallback;
  return Math.min(Math.max(1, Math.floor(base)), max);
}

/** UTF-8 byte length of a string (matches the wire/body budget). */
export function utf8ByteLength(text: string): number {
  // TextEncoder is precise for UTF-8 byte length and avoids Buffer in the
  // transport's pure module surface.
  return new TextEncoder().encode(text).byteLength;
}

function concatBytes(chunks: Uint8Array[]): Uint8Array {
  const total = chunks.reduce((n, c) => n + c.byteLength, 0);
  const out = new Uint8Array(total);
  let offset = 0;
  for (const chunk of chunks) {
    out.set(chunk, offset);
    offset += chunk.byteLength;
  }
  return out;
}

/**
 * Parse a bounded MCP response body: plain JSON or SSE stream. Collects every
 * JSON-RPC message (comments/blank lines ignored, multi-line data joined with
 * "\n"), requires objects with jsonrpc "2.0", and locates the one whose id
 * equals `expectedId`. A mismatched message is never accepted merely because
 * it is the only/last one.
 */
function parseMcpBody(text: string, expectedId: unknown): ParsedBody {
  const scanned = scanRpcMessages(text);
  if (scanned.kind === "malformed") return { kind: "malformed" };
  const rpc = scanned.messages.filter(
    (m): m is Record<string, unknown> =>
      m !== null && typeof m === "object" && !Array.isArray(m) && (m as Record<string, unknown>).jsonrpc === "2.0",
  );
  if (rpc.length === 0) return { kind: "malformed" };
  const hit = rpc.find((m) => m.id === expectedId);
  if (!hit) return { kind: "id_mismatch", parsed: rpc.length };
  if (hit.error !== undefined) {
    const error = hit.error as unknown;
    return {
      kind: "rpc_error",
      error:
        error !== null && typeof error === "object" && !Array.isArray(error)
          ? (error as { code?: unknown })
          : { code: null },
    };
  }
  return { kind: "result", result: (hit as Record<string, unknown>).result };
}

type ScanOutcome = { kind: "ok"; messages: unknown[] } | { kind: "malformed" };

/** Extract all JSON payloads from a body that is JSON or SSE. */
function scanRpcMessages(text: string): ScanOutcome {
  const trimmed = text.trim();
  if (trimmed.length === 0) return { kind: "ok", messages: [] };

  // Plain JSON response (single object).
  if (trimmed.startsWith("{")) {
    try {
      return { kind: "ok", messages: [JSON.parse(trimmed)] };
    } catch {
      // Not plain JSON — fall through to the SSE scan (comments/blank lines
      // may precede data lines).
    }
  }

  const messages: unknown[] = [];
  let dataLines: string[] = [];
  const flush = (): boolean => {
    if (dataLines.length === 0) return true;
    const joined = dataLines.join("\n");
    dataLines = [];
    try {
      messages.push(JSON.parse(joined));
      return true;
    } catch {
      return false;
    }
  };

  for (const rawLine of text.split(/\r?\n/)) {
    if (rawLine.length === 0) {
      if (!flush()) return { kind: "malformed" };
      continue;
    }
    if (rawLine.startsWith(":")) continue; // SSE comment
    if (rawLine.startsWith("data:")) {
      dataLines.push(rawLine.slice(5).replace(/^ /, ""));
      continue;
    }
    if (rawLine.startsWith("event:") || rawLine.startsWith("id:") || rawLine.startsWith("retry:")) {
      continue; // SSE metadata we do not need
    }
    // Ignore any other field line; a bare stray line never terminates an event.
  }
  if (!flush()) return { kind: "malformed" };
  return { kind: "ok", messages };
}