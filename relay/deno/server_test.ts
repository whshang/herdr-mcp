import {
  bindRelaySockets,
  connectUpstreamWebSocket,
  EXPECTED_RUNTIME_CONTRACT_EPOCH,
  EXPECTED_RUNTIME_CONTRACT_HASH,
  extractFrameByteLength,
  handleRequest,
  MAX_FRAME_BYTES,
  RELAY_SERVICE_NAME,
  validateRelayRequest,
  verifyUpstreamHealth,
} from "./server.ts";

function assertEquals(
  actual: unknown,
  expected: unknown,
  message?: string,
): void {
  const actualJson = JSON.stringify(actual);
  const expectedJson = JSON.stringify(expected);
  if (actualJson !== expectedJson) {
    throw new Error(
      message ??
        `assertEquals failed: expected ${expectedJson}, got ${actualJson}`,
    );
  }
}

Deno.test("deno.json locks dynamic deploy runtime and entrypoint", async () => {
  const text = await Deno.readTextFile(new URL("./deno.json", import.meta.url));
  const config = JSON.parse(text);
  assertEquals(config.deploy?.org, "herdr-mcp");
  assertEquals(config.deploy?.app, "relay");
  assertEquals(config.deploy?.runtime?.type, "dynamic");
  assertEquals(config.deploy?.runtime?.entrypoint, "./server.ts");
});

Deno.test("health endpoint returns ok JSON", async () => {
  const req = new Request("https://relay.herdr-mcp.deno.net/health", {
    method: "GET",
  });
  const res = await handleRequest(req);
  assertEquals(res.status, 200);
  const json = await res.json();
  assertEquals(json.status, "ok");
  assertEquals(json.service, RELAY_SERVICE_NAME);
});

Deno.test("rejects non-GET methods", async () => {
  const req = new Request(
    "https://relay.herdr-mcp.deno.net/v1/worker.workers.dev/ws/dev_01ARZ3NDEKTSV4RRFFQ69G5FAV",
    {
      method: "POST",
    },
  );
  const res = await handleRequest(req);
  assertEquals(res.status, 405);
});

Deno.test("rejects non-WebSocket requests without Upgrade header", async () => {
  const req = new Request(
    "https://relay.herdr-mcp.deno.net/v1/worker.workers.dev/ws/dev_01ARZ3NDEKTSV4RRFFQ69G5FAV",
    {
      method: "GET",
    },
  );
  const res = await handleRequest(req);
  assertEquals(res.status, 426);
});

Deno.test("validateRelayRequest accepts canonical and legacy workstation ids with path-based host", () => {
  const url = new URL(
    "https://relay.herdr-mcp.deno.net/v1/my-team.workers.dev/ws/dev_01ARZ3NDEKTSV4RRFFQ69G5FAV",
  );
  const headers = new Headers({
    "sec-websocket-protocol": "herdr-link.v1, herdr-auth.deadbeef0123",
  });
  const res = validateRelayRequest(url, headers);
  assertEquals(res.ok, true);
  if (res.ok) {
    assertEquals(res.deviceId, "dev_01ARZ3NDEKTSV4RRFFQ69G5FAV");
    assertEquals(res.upstreamHost, "my-team.workers.dev");
    assertEquals(
      res.upstreamWsUrl,
      "wss://my-team.workers.dev/ws/dev_01ARZ3NDEKTSV4RRFFQ69G5FAV",
    );
    assertEquals(res.protocols, ["herdr-link.v1", "herdr-auth.deadbeef0123"]);
  }

  const legacy = validateRelayRequest(
    new URL(
      "https://relay.herdr-mcp.deno.net/v1/my-team.workers.dev/ws/prod-real-runtime",
    ),
    headers,
  );
  assertEquals(legacy.ok, true);
  if (legacy.ok) assertEquals(legacy.deviceId, "prod-real-runtime");
});

Deno.test("validateRelayRequest rejects non-workers.dev upstream hosts in path", () => {
  const headers = new Headers({
    "sec-websocket-protocol": "herdr-link.v1, herdr-auth.deadbeef0123",
  });

  const disallowed = [
    "https://relay.herdr-mcp.deno.net/v1/evil.com/ws/dev_01ARZ3NDEKTSV4RRFFQ69G5FAV",
    "https://relay.herdr-mcp.deno.net/v1/workers.dev/ws/dev_01ARZ3NDEKTSV4RRFFQ69G5FAV",
    "https://relay.herdr-mcp.deno.net/v1/192.168.1.1/ws/dev_01ARZ3NDEKTSV4RRFFQ69G5FAV",
    "https://relay.herdr-mcp.deno.net/v1/target.workers.dev:8443/ws/dev_01ARZ3NDEKTSV4RRFFQ69G5FAV",
  ];

  for (const raw of disallowed) {
    const res = validateRelayRequest(new URL(raw), headers);
    assertEquals(res.ok, false, `Expected rejection for: ${raw}`);
    if (!res.ok) {
      assertEquals(res.code, "invalid_upstream_host");
    }
  }
});

Deno.test("validateRelayRequest rejects query-string target overrides", () => {
  const headers = new Headers({
    "sec-websocket-protocol": "herdr-link.v1, herdr-auth.deadbeef0123",
  });
  const url = new URL(
    "https://relay.herdr-mcp.deno.net/v1/target.workers.dev/ws/dev_01ARZ3NDEKTSV4RRFFQ69G5FAV?upstream=evil.workers.dev",
  );
  const res = validateRelayRequest(url, headers);
  assertEquals(res.ok, false);
  if (!res.ok) assertEquals(res.code, "query_not_allowed");
});

Deno.test("validateRelayRequest rejects invalid workstation IDs", () => {
  const headers = new Headers({
    "sec-websocket-protocol": "herdr-link.v1, herdr-auth.deadbeef0123",
  });

  const badIds = [
    "-starts-with-dash",
    "contains space",
    "contains@userinfo",
    "x".repeat(65),
  ];

  for (const id of badIds) {
    const url = new URL(
      `https://relay.herdr-mcp.deno.net/v1/target.workers.dev/ws/${id}`,
    );
    const res = validateRelayRequest(url, headers);
    assertEquals(res.ok, false);
    if (!res.ok) {
      assertEquals(res.code, "invalid_device_id");
    }
  }
});

Deno.test("validateRelayRequest rejects malformed auth protocol formats without echoing secret", () => {
  const url = new URL(
    "https://relay.herdr-mcp.deno.net/v1/target.workers.dev/ws/dev_01ARZ3NDEKTSV4RRFFQ69G5FAV",
  );

  const badAuth = [
    "herdr-link.v1, herdr-auth.", // empty suffix
    "herdr-link.v1, herdr-auth.abc", // odd length (3 chars)
    "herdr-link.v1, herdr-auth.deadbeefg1", // non-hex character 'g'
    `herdr-link.v1, herdr-auth.${"a".repeat(1026)}`, // exceeds 1024 hex length
  ];

  for (const p of badAuth) {
    const headers = new Headers({ "sec-websocket-protocol": p });
    const res = validateRelayRequest(url, headers);
    assertEquals(res.ok, false, `Expected rejection for: ${p}`);
    if (!res.ok) {
      assertEquals(res.code, "invalid_auth_protocol_format");
      // Security: verify error message does not echo the input credentials
      assertEquals(res.message.includes("deadbeef"), false);
      assertEquals(res.message.includes("herdr-auth."), false);
    }
  }
});

Deno.test("validateRelayRequest rejects missing, duplicate, or extra subprotocols", () => {
  const url = new URL(
    "https://relay.herdr-mcp.deno.net/v1/target.workers.dev/ws/dev_01ARZ3NDEKTSV4RRFFQ69G5FAV",
  );

  const badProtocols = [
    undefined,
    "",
    "herdr-link.v1", // missing auth
    "herdr-auth.0123", // missing link
    "herdr-link.v1, herdr-auth.0123, extra.protocol", // 3 protocols
    "herdr-link.v1, herdr-link.v1, herdr-auth.0123", // duplicate link
    "herdr-link.v1, herdr-auth.0123, herdr-auth.4567", // duplicate auth
    "other-link.v1, herdr-auth.0123",
  ];

  for (const p of badProtocols) {
    const headers = new Headers();
    if (p !== undefined) {
      headers.set("sec-websocket-protocol", p);
    }
    const res = validateRelayRequest(url, headers);
    assertEquals(res.ok, false, `Expected rejection for protocol: ${p}`);
  }
});

Deno.test("verifyUpstreamHealth requires status 200 and verified service name", async () => {
  // 1. Success mock
  const mockSuccess = () =>
    new Response(
      JSON.stringify({
        service: "herdr-edge-prod",
        runtimeContractEpoch: EXPECTED_RUNTIME_CONTRACT_EPOCH,
        runtimeContractHash: EXPECTED_RUNTIME_CONTRACT_HASH,
      }),
      { status: 200 },
    );
  const okResult = await verifyUpstreamHealth("target.workers.dev", {
    mockFetch: mockSuccess as any,
  });
  assertEquals(okResult.ok, true);

  // 2. Non-Herdr service rejected
  const mockWrongService = () =>
    new Response(
      JSON.stringify({
        service: "some-other-app",
        contractEpoch: 2,
        contractHash: "sha256:abc",
      }),
      { status: 200 },
    );
  const badServiceResult = await verifyUpstreamHealth("target.workers.dev", {
    mockFetch: mockWrongService as any,
  });
  assertEquals(badServiceResult.ok, false);

  // 3. Herdr-shaped service with an incompatible runtime contract is rejected.
  const mockWrongContract = () =>
    new Response(
      JSON.stringify({
        service: "herdr-edge-prod",
        runtimeContractEpoch: EXPECTED_RUNTIME_CONTRACT_EPOCH,
        runtimeContractHash: "sha256:incompatible",
      }),
      { status: 200 },
    );
  const badContractResult = await verifyUpstreamHealth("target.workers.dev", {
    mockFetch: mockWrongContract as any,
  });
  assertEquals(badContractResult.ok, false);

  // 4. HTTP 500 / 404 rejected
  const mock500 = () => new Response("Internal Error", { status: 500 });
  const fail500Result = await verifyUpstreamHealth("target.workers.dev", {
    mockFetch: mock500 as any,
  });
  assertEquals(fail500Result.ok, false);
});

Deno.test("extractFrameByteLength handles string, ArrayBuffer, ArrayBufferView, and Blob correctly", () => {
  assertEquals(extractFrameByteLength("hello"), 5);
  assertEquals(extractFrameByteLength(new Uint8Array([1, 2, 3])), 3);
  assertEquals(extractFrameByteLength(new ArrayBuffer(8)), 8);
  const blob = new Blob(["test_blob_content"]);
  assertEquals(extractFrameByteLength(blob), 17);
});

class MockWebSocket {
  binaryType = "blob";
  protocol = "";
  readyState = WebSocket.CONNECTING;
  sentMessages: (string | ArrayBuffer | ArrayBufferView | Blob)[] = [];
  closed = false;
  closeCode?: number;
  closeReason?: string;

  onopen: ((e: Event) => void) | null = null;
  onmessage: ((e: MessageEvent) => void) | null = null;
  onclose: ((e: CloseEvent) => void) | null = null;
  onerror: ((e: Event) => void) | null = null;

  send(data: string | ArrayBuffer | ArrayBufferView | Blob) {
    this.sentMessages.push(data);
  }

  close(code?: number, reason?: string) {
    this.closed = true;
    this.closeCode = code;
    this.closeReason = reason;
  }
}

Deno.test("connectUpstreamWebSocket fails if upstream rejects connection or protocols", async () => {
  const upstreamWs = new MockWebSocket();

  const promise = connectUpstreamWebSocket(
    "wss://target.workers.dev/ws/dev_01ARZ3NDEKTSV4RRFFQ69G5FAV",
    ["herdr-link.v1", "herdr-auth.0123"],
    () => upstreamWs as unknown as WebSocket,
  );

  // Simulate upstream error / close before open
  upstreamWs.onerror?.(new Event("error"));
  upstreamWs.onclose?.(new CloseEvent("close", { code: 1006 }));

  const res = await promise;
  assertEquals(res.ok, false);

  const missingProtocolWs = new MockWebSocket();
  const missingProtocolPromise = connectUpstreamWebSocket(
    "wss://target.workers.dev/ws/dev_01ARZ3NDEKTSV4RRFFQ69G5FAV",
    ["herdr-link.v1", "herdr-auth.0123"],
    () => missingProtocolWs as unknown as WebSocket,
  );
  missingProtocolWs.readyState = WebSocket.OPEN;
  missingProtocolWs.onopen?.(new Event("open"));
  const missingProtocolResult = await missingProtocolPromise;
  assertEquals(missingProtocolResult.ok, false);
});

Deno.test("bindRelaySockets forwards bidirectionally and enforces 1 MiB frame limit", () => {
  const clientWs = new MockWebSocket();
  clientWs.readyState = WebSocket.OPEN;

  const upstreamWs = new MockWebSocket();
  upstreamWs.readyState = WebSocket.OPEN;

  bindRelaySockets(
    clientWs as unknown as WebSocket,
    upstreamWs as unknown as WebSocket,
  );

  // 1. Normal bidirectional message
  clientWs.onmessage?.(
    new MessageEvent("message", { data: '{"kind":"hello"}' }),
  );
  assertEquals(upstreamWs.sentMessages, ['{"kind":"hello"}']);

  upstreamWs.onmessage?.(
    new MessageEvent("message", { data: '{"kind":"hello_ack"}' }),
  );
  assertEquals(clientWs.sentMessages, ['{"kind":"hello_ack"}']);

  // 2. Oversized frame trigger close 1009
  const oversized = "x".repeat(MAX_FRAME_BYTES + 10);
  clientWs.onmessage?.(new MessageEvent("message", { data: oversized }));
  assertEquals(clientWs.closed, true);
  assertEquals(clientWs.closeCode, 1009);
  assertEquals(upstreamWs.closed, true);
  assertEquals(upstreamWs.closeCode, 1009);
});

Deno.test("relay expected runtime contract matches authoritative contracts/epoch2.json", async () => {
  const text = await Deno.readTextFile(
    new URL("../../contracts/epoch2.json", import.meta.url),
  );
  const fixture = JSON.parse(text);
  assertEquals(EXPECTED_RUNTIME_CONTRACT_EPOCH, fixture.contract_epoch);
  assertEquals(EXPECTED_RUNTIME_CONTRACT_HASH, fixture.contract_hash);
});
