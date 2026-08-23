#!/usr/bin/env node
/**
 * dev-link-smoke.mjs — optional end-to-end smoke for the Cloudflare dev edge.
 *
 * Requires a locally running `wrangler dev` (see edge/cloudflare/README.md).
 * Simulates the workstation `herdr-link`: authenticates the WSS upgrade via
 * `Sec-WebSocket-Protocol: herdr-auth.<hex>`, sends a valid canonical Relay v1
 * `hello`, then answers a demo `tools/call` with a `tool_result` so the
 * forwarded request settles.
 *
 * This speaks the canonical Relay Protocol v1 wire (snake_case, protocol
 * version 1, kinds hello/hello_ack/heartbeat/status/tool_request/tool_result/
 * tool_error/cancel/cancel_ack). It deliberately reimplements nothing of
 * `src/link/**` (owned elsewhere).
 *
 * Run:
 *   cd edge/cloudflare
 *   npx wrangler dev                          # terminal 1
 *   node tests/manual/dev-link-smoke.mjs      # terminal 2
 */

const EDGE = process.env.EDGE_URL ?? "http://127.0.0.1:8787";
const WORKSTATION_ID = process.env.DEMO_WORKSTATION_ID ?? "dev-ws1";
const SECRET = process.env.LINK_SHARED_SECRET ?? "dev-only-link-secret-change-me";

function exit(msg) {
  console.error(`smoke: ${msg}`);
  process.exit(1);
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

function authProtocol(secret) {
  let hex = "";
  for (const byte of new TextEncoder().encode(secret)) hex += byte.toString(16).padStart(2, "0");
  return `herdr-auth.${hex}`;
}

async function main() {
  // 1) /health on the stable edge role.
  const healthResp = await fetch(`${EDGE}/health`);
  const health = await healthResp.json();
  console.log("health:", healthResp.status, JSON.stringify(health));
  if (health.stage !== "dev-scaffold") exit("unexpected health stage");

  // 2) Upgrade to the workstation DO with link auth via Sec-WebSocket-Protocol.
  const wsUrl = EDGE.replace(/^http/, "ws") + `/ws/${WORKSTATION_ID}`;
  const ws = new WebSocket(wsUrl, ["herdr-link.v1", authProtocol(SECRET)]);
  const helloAck = await new Promise((resolve, reject) => {
    ws.addEventListener("open", () =>
      ws.send(
        JSON.stringify({
          protocol_version: 1,
          kind: "hello",
          workstation_id: WORKSTATION_ID,
          boot_id: "smoke-boot",
          link_version: "0.0.1-smoke",
          connected_at_ms: Date.now(),
          capabilities: ["herdr", "fs"],
          runtime: {
            runtime_version: "0.3.26-smoke",
            runtime_commit: "smoke",
            runtime_generation: "g-smoke",
            contract_epoch: 1,
            contract_hash: "sha256:3f23083ae31b977dad21b1ec9d6919c49e1067a27f7b7eea7bdd021b54770c0d",
            herdr_version: null,
            herdr_protocol: null,
          },
        }),
      ),
    );
    ws.addEventListener("message", (ev) => resolve(JSON.parse(ev.data)));
    ws.addEventListener("error", () => reject(new Error("ws error during hello")));
    setTimeout(() => reject(new Error("timed out waiting for hello_ack")), 5000);
  });
  console.log("hello_ack:", JSON.stringify(helloAck));
  if (!helloAck.ok) exit("no hello_ack");

  // 3) Arm the tool_request waiter, then issue the demo tools/call.
  let resolveRequest;
  const requestFrame = new Promise((resolve) => {
    resolveRequest = resolve;
  });
  ws.addEventListener("message", (ev) => {
    const msg = JSON.parse(ev.data);
    if (msg.kind === "tool_request") resolveRequest(msg);
  });

  const callPromise = fetch(`${EDGE}/mcp`, {
    method: "POST",
    headers: {
      "content-type": "application/json",
      "x-herdr-workstation": WORKSTATION_ID,
    },
    body: JSON.stringify({
      jsonrpc: "2.0",
      id: 1,
      method: "tools/call",
      params: { name: "herdr_inspect", arguments: { path: "/" } },
    }),
  }).then(async (r) => ({ status: r.status, body: await r.json() }));

  // 4) Answer the tool_request like a link runtime would.
  const answer = await Promise.race([
    requestFrame,
    new Promise((_, reject) => setTimeout(() => reject(new Error("timed out waiting for tool_request frame")), 5000)),
  ]);
  ws.send(
    JSON.stringify({
      protocol_version: 1,
      kind: "tool_result",
      workstation_id: WORKSTATION_ID,
      request_id: answer.request_id,
      result: { demoServed: true, operation: answer.operation },
      served_at_ms: Date.now(),
      runtime_generation: "g-smoke",
    }),
  );

  const call = await callPromise;
  console.log("tools/call:", call.status, JSON.stringify(call.body));
  if (call.body?.jsonrpc !== "2.0" || call.body?.id !== 1) exit("invalid JSON-RPC response");
  if (call.body?.result?.isError === true) exit("demo call returned MCP tool error");
  if (call.body?.result?.structuredContent?.demoServed !== true) exit("demo call did not resolve ok");
  console.log("smoke OK");
  ws.close();
  process.exit(0);
}

main().catch((e) => exit(e.message));