/**
 * Herdr Relay v1 — Stateless Deno Reference Reverse Proxy
 *
 * Dedicated narrow authenticated reverse proxy for Herdr Link WebSocket connections.
 * Forwards WSS traffic exclusively to verified Herdr *.workers.dev upstreams.
 *
 * Protocol & Security Contract:
 *   1. Upstream-first: The relay connects and handshakes with the upstream Worker
 *      WebSocket BEFORE upgrading the client. If upstream rejects auth or fails,
 *      the client receives HTTP 502/401, never a false 101 Switching Protocols.
 *   2. Upstream Verification: Before WS connection, performs GET https://<host>/health
 *      with redirect: manual to verify `service: "herdr-edge-prod"`.
 *   3. Strict Path: /v1/<workers.dev-host>/ws/<Herdr workstation identifier>
 *   4. Subprotocols: Exactly `herdr-link.v1` and `herdr-auth.<hex>` (even length, <= 1024 hex).
 *   5. Frame budget: Strict 1 MiB max frame size. Oversized frames trigger close 1009.
 */

export const RELAY_SERVICE_NAME = "herdr-relay-deno";
export const RELAY_SERVICE_VERSION = "0.4.5";
export const LINK_SUBPROTOCOL = "herdr-link.v1";
export const AUTH_SUBPROTOCOL_PREFIX = "herdr-auth.";
export const WORKERS_DEV_SUFFIX = ".workers.dev";
export const MAX_FRAME_BYTES = 1024 * 1024; // 1 MiB
export const MAX_AUTH_HEX_LEN = 1024; // 512 bytes * 2 hex chars
export const HEALTH_PROBE_TIMEOUT_MS = 3_000;
export const MAX_HEALTH_RESPONSE_BYTES = 64 * 1024;
export const EXPECTED_RUNTIME_CONTRACT_EPOCH = 2;
export const EXPECTED_RUNTIME_CONTRACT_HASH =
  "sha256:7da23ad2ec8e7703d6380062126ba797218bde9e7711138c6b3e0ca6592efbf8";

// Relay Protocol v1 workstation identity grammar. This intentionally accepts
// legacy installed ids such as `prod-real-runtime` as well as canonical
// `dev_<ULID>` ids; the upstream Worker credential is still authoritative.
const WORKSTATION_ID_REGEX = /^[A-Za-z0-9][A-Za-z0-9._:-]{0,63}$/;
// Subdomain of workers.dev: labels containing [a-z0-9-], ending with .workers.dev
const WORKERS_DEV_HOST_REGEX =
  /^[a-z0-9]([a-z0-9-]*[a-z0-9])?(\.[a-z0-9]([a-z0-9-]*[a-z0-9])?)*\.workers\.dev$/i;
// Valid hex string: even length, <= 1024 chars
const AUTH_HEX_REGEX = /^[0-9a-fA-F]+$/;

export interface ValidationSuccess {
  ok: true;
  deviceId: string;
  upstreamHost: string;
  upstreamWsUrl: string;
  authProtocol: string;
  protocols: [string, string];
}

export interface ValidationFailure {
  ok: false;
  status: number;
  code: string;
  message: string;
}

export type ValidationResult = ValidationSuccess | ValidationFailure;

/**
 * Validate incoming Relay request parameters without logging or echoing credentials.
 */
export function validateRelayRequest(
  url: URL,
  headers: Headers,
  options?: { allowLoopbackForTest?: boolean; mockUpstreamPort?: number },
): ValidationResult {
  if (url.search !== "") {
    return {
      ok: false,
      status: 400,
      code: "query_not_allowed",
      message: "Relay target must be encoded only in the canonical path",
    };
  }

  // Path shape: /v1/<upstream-host>/ws/<device_id>
  const pathname = url.pathname.replace(/\/+$/, "");
  const segments = pathname.split("/").filter(Boolean);
  if (
    segments.length !== 4 || segments[0] !== "v1" || segments[2] !== "ws"
  ) {
    return {
      ok: false,
      status: 400,
      code: "invalid_path",
      message: "Path must match /v1/<workers.dev-host>/ws/<workstation-id>",
    };
  }

  const rawHost = segments[1].trim().toLowerCase();
  const deviceId = segments[3];

  if (!WORKSTATION_ID_REGEX.test(deviceId)) {
    return {
      ok: false,
      status: 400,
      code: "invalid_device_id",
      message: "Workstation ID format is invalid",
    };
  }

  // Validate upstream host
  const isLoopbackTest = options?.allowLoopbackForTest &&
    (rawHost === "127.0.0.1" || rawHost === "localhost");
  if (!isLoopbackTest) {
    if (
      !rawHost.endsWith(WORKERS_DEV_SUFFIX) ||
      rawHost === "workers.dev" ||
      rawHost.includes(":") ||
      rawHost.includes("@") ||
      rawHost.includes("/") ||
      !WORKERS_DEV_HOST_REGEX.test(rawHost)
    ) {
      return {
        ok: false,
        status: 403,
        code: "invalid_upstream_host",
        message:
          "Upstream host must be a valid *.workers.dev domain with no port or credentials",
      };
    }
  }

  // Subprotocols check
  const protocolHeader = headers.get("sec-websocket-protocol");
  if (!protocolHeader) {
    return {
      ok: false,
      status: 400,
      code: "missing_subprotocol",
      message: "Missing Sec-WebSocket-Protocol header",
    };
  }

  const rawProtocols = protocolHeader.split(",").map((s) => s.trim()).filter(
    Boolean,
  );
  let hasLinkProtocol = false;
  let authProtocol: string | null = null;
  let extraProtocols = false;

  for (const proto of rawProtocols) {
    if (proto === LINK_SUBPROTOCOL) {
      if (hasLinkProtocol) {
        extraProtocols = true; // Duplicate link protocol
      }
      hasLinkProtocol = true;
    } else if (proto.startsWith(AUTH_SUBPROTOCOL_PREFIX)) {
      if (authProtocol !== null) {
        extraProtocols = true; // Duplicate auth protocol
      }
      const hex = proto.slice(AUTH_SUBPROTOCOL_PREFIX.length);
      // Validate hex format, even length, bounded length (parity with decodeLinkAuthProtocol)
      if (
        hex.length === 0 ||
        hex.length % 2 !== 0 ||
        hex.length > MAX_AUTH_HEX_LEN ||
        !AUTH_HEX_REGEX.test(hex)
      ) {
        return {
          ok: false,
          status: 400,
          code: "invalid_auth_protocol_format",
          message: "Auth protocol format is invalid",
        };
      }
      authProtocol = proto;
    } else {
      extraProtocols = true;
    }
  }

  if (
    !hasLinkProtocol || !authProtocol || extraProtocols ||
    rawProtocols.length !== 2
  ) {
    return {
      ok: false,
      status: 400,
      code: "invalid_subprotocols",
      message:
        `Subprotocols must contain exactly '${LINK_SUBPROTOCOL}' and a valid '${AUTH_SUBPROTOCOL_PREFIX}<hex>'`,
    };
  }

  const upstreamScheme = isLoopbackTest ? "ws" : "wss";
  const upstreamPortSuffix = isLoopbackTest && options?.mockUpstreamPort
    ? `:${options.mockUpstreamPort}`
    : "";
  const upstreamWsUrl =
    `${upstreamScheme}://${rawHost}${upstreamPortSuffix}/ws/${
      encodeURIComponent(deviceId)
    }`;

  return {
    ok: true,
    deviceId,
    upstreamHost: rawHost,
    upstreamWsUrl,
    authProtocol,
    protocols: [LINK_SUBPROTOCOL, authProtocol],
  };
}

/**
 * Verify that the target workers.dev host is a genuine Herdr Edge Worker via GET /health.
 */
export async function verifyUpstreamHealth(
  upstreamHost: string,
  options?: {
    allowLoopbackForTest?: boolean;
    mockHealthPort?: number;
    mockFetch?: typeof fetch;
  },
): Promise<{ ok: true } | { ok: false; reason: string }> {
  const isLoopback = options?.allowLoopbackForTest &&
    (upstreamHost === "127.0.0.1" || upstreamHost === "localhost");
  const scheme = isLoopback ? "http" : "https";
  const portSuffix = isLoopback && options?.mockHealthPort
    ? `:${options.mockHealthPort}`
    : "";
  const healthUrl = `${scheme}://${upstreamHost}${portSuffix}/health`;

  const fetchFn = options?.mockFetch ?? fetch;
  const controller = new AbortController();
  const timeoutId = setTimeout(
    () => controller.abort(),
    HEALTH_PROBE_TIMEOUT_MS,
  );

  try {
    const res = await fetchFn(healthUrl, {
      method: "GET",
      redirect: "manual",
      signal: controller.signal,
      headers: {
        "user-agent": "herdr-relay-deno/0.4.5",
        "accept": "application/json",
      },
    });

    clearTimeout(timeoutId);

    if (res.status !== 200) {
      return {
        ok: false,
        reason: `upstream /health returned status ${res.status}`,
      };
    }

    const contentLength = res.headers.get("content-length");
    if (
      contentLength !== null &&
      Number.isFinite(Number(contentLength)) &&
      Number(contentLength) > MAX_HEALTH_RESPONSE_BYTES
    ) {
      return {
        ok: false,
        reason: "upstream /health response exceeded byte limit",
      };
    }

    const reader = res.body?.getReader();
    if (!reader) {
      return { ok: false, reason: "upstream /health response body missing" };
    }
    const chunks: Uint8Array[] = [];
    let totalBytes = 0;
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      totalBytes += value.byteLength;
      if (totalBytes > MAX_HEALTH_RESPONSE_BYTES) {
        await reader.cancel();
        return {
          ok: false,
          reason: "upstream /health response exceeded byte limit",
        };
      }
      chunks.push(value);
    }
    const body = new Uint8Array(totalBytes);
    let offset = 0;
    for (const chunk of chunks) {
      body.set(chunk, offset);
      offset += chunk.byteLength;
    }
    const text = new TextDecoder().decode(body);

    let parsed: Record<string, unknown>;
    try {
      parsed = JSON.parse(text);
    } catch {
      return { ok: false, reason: "upstream /health returned non-JSON" };
    }

    // Must be a recognized Herdr Edge service
    const service = typeof parsed.service === "string" ? parsed.service : "";
    if (service !== "herdr-edge-prod") {
      return {
        ok: false,
        reason: "upstream is not a verified Herdr Edge service",
      };
    }

    const runtimeEpoch = parsed.runtimeContractEpoch ?? parsed.contractEpoch;
    const runtimeHash = parsed.runtimeContractHash ?? parsed.contractHash;
    if (
      runtimeEpoch !== EXPECTED_RUNTIME_CONTRACT_EPOCH ||
      runtimeHash !== EXPECTED_RUNTIME_CONTRACT_HASH
    ) {
      return {
        ok: false,
        reason: "upstream runtime contract is incompatible with this relay",
      };
    }

    return { ok: true };
  } catch {
    clearTimeout(timeoutId);
    return {
      ok: false,
      reason: "upstream /health probe failed",
    };
  }
}

export function extractFrameByteLength(
  data: string | ArrayBuffer | ArrayBufferView | Blob,
): number {
  if (typeof data === "string") {
    return new TextEncoder().encode(data).length;
  }
  if (data instanceof ArrayBuffer) {
    return data.byteLength;
  }
  if (ArrayBuffer.isView(data)) {
    return data.byteLength;
  }
  if (typeof Blob !== "undefined" && data instanceof Blob) {
    return data.size;
  }
  return 0;
}

export interface UpstreamConnectResult {
  ok: boolean;
  ws?: WebSocket;
  errorReason?: string;
}

/**
 * Connect to upstream WebSocket and wait for open event before client is accepted.
 */
export function connectUpstreamWebSocket(
  upstreamWsUrl: string,
  protocols: [string, string],
  upstreamFactory: (url: string, protos: string[]) => WebSocket,
  timeoutMs = 5000,
): Promise<UpstreamConnectResult> {
  return new Promise((resolve) => {
    let settled = false;
    const ws = upstreamFactory(upstreamWsUrl, protocols);
    try {
      ws.binaryType = "arraybuffer";
    } catch {
      // ignore
    }

    const timer = setTimeout(() => {
      if (!settled) {
        settled = true;
        try {
          ws.close(1011, "upstream connect timeout");
        } catch {
          // ignore
        }
        resolve({ ok: false, errorReason: "upstream handshake timeout" });
      }
    }, timeoutMs);

    ws.onopen = () => {
      if (!settled) {
        settled = true;
        clearTimeout(timer);
        // Verify selected protocol
        if (ws.protocol !== LINK_SUBPROTOCOL) {
          try {
            ws.close(1002, "protocol mismatch");
          } catch {
            // ignore
          }
          resolve({ ok: false, errorReason: "protocol mismatch" });
          return;
        }
        resolve({ ok: true, ws });
      }
    };

    ws.onerror = () => {
      if (!settled) {
        settled = true;
        clearTimeout(timer);
        resolve({ ok: false, errorReason: "upstream connection error" });
      }
    };

    ws.onclose = (event) => {
      if (!settled) {
        settled = true;
        clearTimeout(timer);
        resolve({
          ok: false,
          errorReason: `upstream closed before open code=${event.code}`,
        });
      }
    };
  });
}

/**
 * Wire bidirectional frames between client and already-open upstream WebSocket.
 */
export function bindRelaySockets(
  clientWs: WebSocket,
  upstreamWs: WebSocket,
): void {
  try {
    clientWs.binaryType = "arraybuffer";
  } catch {
    // ignore
  }

  let clientClosed = false;
  let upstreamClosed = false;

  function safeClose(ws: WebSocket, code: number, reason: string) {
    try {
      if (
        ws.readyState === WebSocket.OPEN ||
        ws.readyState === WebSocket.CONNECTING
      ) {
        ws.close(code, reason.slice(0, 120));
      }
    } catch {
      // ignore
    }
  }

  clientWs.onmessage = (event) => {
    const data = event.data;
    const byteLength = extractFrameByteLength(data);

    // Frame cap check: 1 MiB
    if (byteLength > MAX_FRAME_BYTES) {
      safeClose(clientWs, 1009, "frame exceeded 1 MiB limit");
      safeClose(upstreamWs, 1009, "frame exceeded 1 MiB limit");
      return;
    }

    if (upstreamWs.readyState === WebSocket.OPEN) {
      try {
        upstreamWs.send(data);
      } catch {
        safeClose(clientWs, 1011, "upstream send failed");
        safeClose(upstreamWs, 1011, "upstream send failed");
      }
    }
  };

  upstreamWs.onmessage = (event) => {
    const data = event.data;
    const byteLength = extractFrameByteLength(data);

    if (byteLength > MAX_FRAME_BYTES) {
      safeClose(clientWs, 1009, "frame exceeded 1 MiB limit");
      safeClose(upstreamWs, 1009, "frame exceeded 1 MiB limit");
      return;
    }

    if (clientWs.readyState === WebSocket.OPEN) {
      try {
        clientWs.send(data);
      } catch {
        safeClose(clientWs, 1011, "client send failed");
        safeClose(upstreamWs, 1011, "client send failed");
      }
    }
  };

  clientWs.onclose = (event) => {
    clientClosed = true;
    if (!upstreamClosed) {
      upstreamClosed = true;
      safeClose(
        upstreamWs,
        event.code || 1000,
        event.reason || "client closed",
      );
    }
  };

  upstreamWs.onclose = (event) => {
    upstreamClosed = true;
    if (!clientClosed) {
      clientClosed = true;
      safeClose(
        clientWs,
        event.code || 1000,
        event.reason || "upstream closed",
      );
    }
  };

  clientWs.onerror = () => {
    if (!upstreamClosed) {
      upstreamClosed = true;
      safeClose(upstreamWs, 1011, "client socket error");
    }
  };

  upstreamWs.onerror = () => {
    if (!clientClosed) {
      clientClosed = true;
      safeClose(clientWs, 1011, "upstream socket error");
    }
  };
}

/**
 * Handle incoming HTTP & WebSocket requests in Deno.
 */
export async function handleRequest(
  req: Request,
  options?: {
    allowLoopbackForTest?: boolean;
    mockUpstreamPort?: number;
    mockHealthPort?: number;
    mockFetch?: typeof fetch;
    mockUpstreamFactory?: (url: string, protocols: string[]) => WebSocket;
  },
): Promise<Response> {
  const url = new URL(req.url);

  // Health endpoint
  if (
    req.method === "GET" &&
    (url.pathname === "/health" || url.pathname === "/healthz")
  ) {
    return new Response(
      JSON.stringify({
        status: "ok",
        service: RELAY_SERVICE_NAME,
        version: RELAY_SERVICE_VERSION,
      }),
      {
        status: 200,
        headers: {
          "content-type": "application/json",
          "cache-control": "no-store",
        },
      },
    );
  }

  // Reject non-GET
  if (req.method !== "GET") {
    return new Response("Method Not Allowed", { status: 405 });
  }

  // WebSocket upgrade check
  const upgrade = req.headers.get("upgrade");
  if (!upgrade || upgrade.toLowerCase() !== "websocket") {
    return new Response("WebSocket Upgrade Required", { status: 426 });
  }

  // Validate parameters & subprotocols
  const validation = validateRelayRequest(url, req.headers, options);
  if (!validation.ok) {
    return new Response(
      JSON.stringify({
        ok: false,
        error: validation.code,
        message: validation.message,
      }),
      {
        status: validation.status,
        headers: { "content-type": "application/json" },
      },
    );
  }

  // 1. Verify upstream target is a genuine Herdr Edge via GET /health
  const healthCheck = await verifyUpstreamHealth(
    validation.upstreamHost,
    options,
  );
  if (!healthCheck.ok) {
    return new Response(
      JSON.stringify({
        ok: false,
        error: "upstream_health_failed",
        message: "Upstream health verification failed",
      }),
      {
        status: 502,
        headers: { "content-type": "application/json" },
      },
    );
  }

  // 2. Connect to upstream WebSocket first (upstream-first invariant)
  const upstreamFactory = options?.mockUpstreamFactory ??
    ((wsUrl: string, protos: string[]) => new WebSocket(wsUrl, protos));

  const upstreamResult = await connectUpstreamWebSocket(
    validation.upstreamWsUrl,
    validation.protocols,
    upstreamFactory,
  );

  if (!upstreamResult.ok || !upstreamResult.ws) {
    return new Response(
      JSON.stringify({
        ok: false,
        error: "upstream_connect_failed",
        message: upstreamResult.errorReason ||
          "Failed to establish upstream WebSocket",
      }),
      {
        status: 502,
        headers: { "content-type": "application/json" },
      },
    );
  }

  // 3. Only after upstream is successfully open and verified, upgrade the client!
  // @ts-ignore: Deno upgradeWebSocket API
  if (
    typeof Deno !== "undefined" && typeof Deno.upgradeWebSocket === "function"
  ) {
    // @ts-ignore: Deno upgradeWebSocket API
    const { response, socket: clientWs } = Deno.upgradeWebSocket(req, {
      protocol: LINK_SUBPROTOCOL,
      idleTimeout: 0, // Inbound idleTimeout=0: Herdr manages heartbeat/reconnect
    });

    bindRelaySockets(clientWs, upstreamResult.ws);
    return response;
  }

  // If runtime cannot upgrade: clean up upstream
  try {
    upstreamResult.ws.close(1011, "server error");
  } catch {
    // ignore
  }

  return new Response("Runtime does not support Deno.upgradeWebSocket", {
    status: 500,
  });
}

// Entrypoint for `deno run` or Deno Deploy
// @ts-ignore: Deno runtime API
if (typeof Deno !== "undefined" && import.meta.main) {
  // @ts-ignore: Deno runtime API
  const port = parseInt(Deno.env.get("PORT") || "8000", 10);
  // @ts-ignore: Deno runtime API
  Deno.serve({ port }, (req: Request) => handleRequest(req));
  console.log(`[herdr-relay-deno] listening on :${port}`);
}
