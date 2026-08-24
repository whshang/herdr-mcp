/**
 * mcp-chatgpt-transport.ts — ChatGPT/OpenAI MCP transport helpers for the
 * Cloudflare Edge.
 *
 * Isolated from routing/auth logic on purpose: dependency-free (only Web
 * platform primitives). index.ts composes these helpers after authentication.
 *
 * Scope boundaries: this module only decides framing/stateless transport.
 * OAuth, DCR and discovery are implemented by the dedicated OAuth modules.
 *
 * Session policy — hard guarantee of this module:
 *  - No Mcp-Session-Id is ever ISSUED (no response header) and none is ever
 *    REQUIRED. The helpers' input surface has no session channel, so a stale
 *    Mcp-Session-Id an upstream router received is ignored by construction.
 *    This mirrors the production root-cause fix: OpenAI's connector stores the
 *    initialize response session id and reuses the stale id after a restart,
 *    surfacing as JSON-RPC -32600 "Session terminated"; serving stateless
 *    end-to-end means there is nothing that can go stale.
 *  - Request credentials / Authorization are never copied into responses.
 *
 * Post framing — matches the exact wire formats proven by production:
 *  - SSE one-event frame `event: message\ndata: <json>\n\n` is the
 *    @modelcontextprotocol/sdk StreamableHTTP framing
 *    (dist/esm/server/webStandardStreamableHttp.js: `event: message\n` +
 *    `data: ${JSON.stringify(message)}\n\n`).
 *  - SSE responses carry `text/event-stream` + `Cache-Control: no-cache,
 *    no-transform` (SDK + production GET probe header, asserted verbatim in
 *    tests/transport.test.mjs).
 *  - `server/discover` is answered with application/json in production
 *    (src/server.ts handleMcpRequest answers discover with res.status(200)
 *    .json(...) BEFORE the stateless branch, so the SDK transport never
 *    renders it). This module follows that proven source; the task text lists
 *    discover under SSE — see `SSE_POST_METHODS` note.
 *  - tools/call is JSON for proxy-safe payloads (production
 *    `enableJsonResponse = method === "tools/call"`).
 *  - Notifications (dev.body === null) keep their status (204) with an empty
 *    body — never a JSON-encoded "null".
 *  - Non-ChatGPT clients may use JSON for every POST.
 */

/** Marker used for User-Agent detection (OpenAI MCP connector). */
export const OPENAI_MCP_UA_MARKER = "openai-mcp" as const;

/** Default GET probe heartbeat interval — matches production's 15s and the SDK's DEFAULT_SSE_KEEP_ALIVE_MS. */
export const DEFAULT_PROBE_HEARTBEAT_MS = 15_000;

/**
 * JSON-RPC methods framed as Streamable-HTTP SSE for ChatGPT/openai-mcp.
 *
 * NOTE on `server/discover`: the task lists it here, but production answers
 * discover with JSON (src/server.ts `res.status(200).json(...)`, before the
 * stateless branch) — proven wire behavior. It therefore deliberately stays
 * out of this set; the Edge router gets JSON for discover.
 */
export const SSE_POST_METHODS: ReadonlySet<string> = new Set<string>(["initialize", "tools/list"]);

export interface SessionlessProbeOptions {
  /** Heartbeat interval ms between `: keepalive\n\n` frames. Default 15_000; injectable for tests. Non-finite/<1 disables the timer. */
  heartbeatMs?: number;
  /** AbortSignal (e.g. request.signal) — abort clears the heartbeat timer even without a body cancel. */
  signal?: AbortSignal;
  /** Optional hook invoked exactly once when the stream is cancelled/aborted and the timer is cleaned up. */
  onCleanup?: () => void;
}

export interface McpResult {
  status: number;
  /** JSON-RPC envelope to serialize, or null for status-only responses (204 notifications). */
  body: Record<string, unknown> | null;
}

export interface SerializeMcpOptions {
  /** Raw User-Agent header value from the incoming request (may be absent). */
  userAgent: string | null | undefined;
  /** Validated OAuth client_id; ChatGPT CIMD uses an HTTPS chatgpt.com URL. */
  oauthClientId?: string | null;
  /** JSON-RPC method of the handled request, e.g. "initialize" / "tools/call". */
  method: string;
}

/**
 * ChatGPT/openai-mcp detection: case-insensitive substring match on
 * `openai-mcp` in the User-Agent (production-observed UA: `openai-mcp/1.0.0`).
 */
export function isOpenAiMcpUserAgent(userAgent: string | null | undefined): boolean {
  if (typeof userAgent !== "string") return false;
  return userAgent.toLowerCase().includes(OPENAI_MCP_UA_MARKER);
}

/** Match the ChatGPT CIMD client_id semantics used by the localhost OAuth server. */
export function isChatgptOAuthClientId(clientId: string | null | undefined): boolean {
  if (typeof clientId !== "string" || !/^https:\/\//i.test(clientId)) return false;
  try {
    const host = new URL(clientId).hostname.toLowerCase();
    return host === "chatgpt.com" || host === "www.chatgpt.com";
  } catch {
    return false;
  }
}

/**
 * True when a POST JSON-RPC result should be framed as Streamable-HTTP SSE.
 * Only ChatGPT/openai-mcp `initialize` + `tools/list`; everything else
 * (tools/call, server/discover, errors, non-ChatGPT UAs) is JSON in dev.
 */
export function shouldFramePostAsSse(options: {
  userAgent: string | null | undefined;
  oauthClientId?: string | null;
  method: string;
}): boolean {
  if (!isOpenAiMcpUserAgent(options.userAgent) && !isChatgptOAuthClientId(options.oauthClientId)) return false;
  return SSE_POST_METHODS.has(options.method);
}

/**
 * Exact Streamable-HTTP one-event SSE frame proven by the SDK:
 * `event: message\ndata: <json>\n\n`
 */
export function sseFrameEvent(payload: unknown): string {
  return `event: message\ndata: ${JSON.stringify(payload)}\n\n`;
}

/**
 * Persistent, sessionless SSE probe for the OpenAI/ChatGPT connector's GET
 * /mcp probe (a NEW conversation GETs before any initialize; production treats
 * an EOF as "transport terminated", so the stream must stay open).
 *
 * - Immediate first frame `: connected\n\n` (never a JSON-RPC error the
 *   connector could misread as invalid_mcp_response).
 * - Then `: keepalive\n\n` every heartbeatMs; heartbeat frames are only
 *   enqueued while a reader is attached (desiredSize > 0) so an idle/aborted
 *   probe cannot buffer unboundedly.
 * - Heartbeat timer is cleared exactly once on stream cancel() or signal
 *   abort; onCleanup() fires once.
 * - Headers match production (src/server.ts): text/event-stream,
 *   Cache-Control: no-cache, no-transform, Connection: keep-alive,
 *   X-Accel-Buffering: no. No Mcp-Session-Id.
 */
export function createSessionlessMcpProbeResponse(options: SessionlessProbeOptions = {}): Response {
  const heartbeatMs = normalizeHeartbeatMs(options.heartbeatMs);
  const encoder = new TextEncoder();
  let closed = false;
  let timer: ReturnType<typeof setInterval> | undefined;

  const clearTimer = (): void => {
    if (timer !== undefined) {
      clearInterval(timer);
      timer = undefined;
    }
    if (!closed) {
      closed = true;
      options.onCleanup?.();
    }
  };

  const stream = new ReadableStream<Uint8Array>({
    start(controller) {
      try {
        controller.enqueue(encoder.encode(": connected\n\n"));
      } catch {
        // No reader yet / stream already closed — nothing to enqueue.
      }
      if (heartbeatMs > 0) {
        timer = setInterval(() => {
          if (closed) return;
          try {
            const desired = controller.desiredSize;
            if (desired !== null && desired > 0) {
              controller.enqueue(encoder.encode(": keepalive\n\n"));
            }
          } catch {
            // Stream closed underneath (client abort) — stop the timer.
            clearTimer();
          }
        }, heartbeatMs);
      }
    },
    cancel() {
      clearTimer();
    },
  });

  if (options.signal !== undefined) {
    if (options.signal.aborted) {
      clearTimer();
    } else {
      options.signal.addEventListener("abort", clearTimer, { once: true });
    }
  }

  return new Response(stream, {
    status: 200,
    headers: {
      "content-type": "text/event-stream",
      "cache-control": "no-cache, no-transform",
      connection: "keep-alive",
      "x-accel-buffering": "no",
    },
  });
}

/**
 * Serialize a handleMcp result (McpResponse is structurally compatible
 * with McpResult) into the transport Response:
 *  - body === null            -> status-only (204 notification), empty body
 *  - ChatGPT initialize/list  -> SSE one-event frame
 *  - everything else          -> application/json, Cache-Control: no-store
 * Never emits or echoes Mcp-Session-Id / credentials.
 */
export function serializeMcpResponse(result: McpResult, options: SerializeMcpOptions): Response {
  if (result.body === null) {
    return new Response(null, { status: result.status });
  }
  if (shouldFramePostAsSse(options)) {
    return new Response(sseFrameEvent(result.body), {
      status: result.status,
      headers: {
        "content-type": "text/event-stream",
        "cache-control": "no-cache, no-transform",
      },
    });
  }
  return new Response(JSON.stringify(result.body), {
    status: result.status,
    headers: {
      "content-type": "application/json",
      "cache-control": "no-store",
    },
  });
}

/** Mirror the SDK's armSseKeepAlive guard: non-finite or <1 disables the timer. */
function normalizeHeartbeatMs(value: number | undefined): number {
  if (value === undefined) return DEFAULT_PROBE_HEARTBEAT_MS;
  if (typeof value !== "number" || !Number.isFinite(value) || value < 1) return 0;
  return Math.min(value, 2 ** 31 - 1);
}