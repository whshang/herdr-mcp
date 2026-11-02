/**
 * Edge → runtime integration coverage for grant-routed `browser_dispatch.stop`
 * (issue #613, follow-up to #611).
 *
 * The device reply is real: `handleMcp` from `edge/cloudflare/dist` forwards
 * through `./edge-runtime-harness.mjs`, which projects the exact Relay Protocol
 * v1 `tool_request` the WorkstationDO would emit and delivers it through the
 * trusted-IPC hop the Rust Link performs to a real `herdr-mcp` candidate runtime
 * built from this checkout. Only the transport between Edge and runtime is
 * replaced; authorization is decided by the Rust runtime.
 */

import { test, after } from "node:test";
import assert from "node:assert/strict";

import { makeLimits } from "../dist/limits.js";
import { handleMcp } from "../dist/mcp-handler.js";
import {
  createEdgeDeps,
  createEdgeToRuntimeTransport,
  runtimeBinarySkipReason,
  startEdgeRuntime,
  callerResultOf,
} from "./edge-runtime-harness.mjs";

const skip = runtimeBinarySkipReason();

const DEVICE_A = "dev_01ARZ3NDEKTSV4RRFFQ69G5FAV";
const DEVICE_B = "dev_01ARZ3NDEKTSV4RRFFQ69G5FAW";
// A canonical explicit-device route uses the device id as the workstation id
// (`resolveRoute` in mcp-handler.ts), so the relay frame is addressed to it.
const WORKSTATION_A = DEVICE_A;
const WORKSTATION_B = DEVICE_B;
// Device B's grant never reaches device A's runtime; these refs are the ones the
// tests assert are absent from everything the runtime receives.
const WEBCHAT_B = `bep_${"b".repeat(64)}`;
const ACCOUNT_B = `br_${"2".repeat(64)}`;
const CONNECTOR_ID = "conn_edge_runtime_harness";

let shared;

/** One real runtime (plus one transport) serves both scenarios. */
async function harness() {
  if (!shared) {
    shared = (async () => {
      const runtime = await startEdgeRuntime({ deviceId: DEVICE_A, profileSeed: "edge-runtime-harness-613" });
      const transport = createEdgeToRuntimeTransport(runtime, { workstationId: WORKSTATION_A });
      return { runtime, transport };
    })();
  }
  return shared;
}

after(async () => {
  if (!shared) return;
  const { runtime } = await shared.catch(() => ({ runtime: undefined }));
  await runtime?.stop();
});

function edgeDeps({ transport, webchatControlGrants, pageAssistGrants = [] }) {
  return createEdgeDeps({
    transport,
    edgeEnv: "test",
    limits: makeLimits(),
    client: {
      connectorId: CONNECTOR_ID,
      grantGeneration: 3,
      webchatControlGrants,
      pageAssistGrants,
    },
    resolveDevice: async (selector) => (selector === DEVICE_A
      ? { ok: true, device_id: DEVICE_A, workstation_id: WORKSTATION_A, routing_reason: "explicit_device_selector" }
      : selector === DEVICE_B
        ? { ok: true, device_id: DEVICE_B, workstation_id: WORKSTATION_B, routing_reason: "explicit_device_selector" }
        : { ok: false, code: "device_not_found" }),
  });
}

async function stopThroughEdge({ deps, device, dispatchId, idempotencyKey, id = 1 }) {
  return handleMcp(
    {
      jsonrpc: "2.0",
      id,
      method: "tools/call",
      params: {
        name: "herdr_call",
        arguments: {
          method: "herdr_mcp.browser_dispatch.stop",
          device,
          params: JSON.stringify({
            dispatch_id: dispatchId,
            expected_generation: 7,
            idempotency_key: idempotencyKey,
          }),
        },
      },
    },
    "w1",
    deps.value,
  );
}

test("user sees a real runtime grant refusal relayed unchanged | Given a routed device with no WebChat control grant | When the Edge forwards herdr_mcp.browser_dispatch.stop | Then the forwarded frame carries no grant context and the runtime's own caller_grant_missing reaches the caller", { skip }, async () => {
  const { runtime, transport } = await harness();
  const before = transport.calls.length;
  const deps = edgeDeps({
    transport,
    // The connector holds a grant, but only for the other enrolled device.
    webchatControlGrants: [{
      device_id: DEVICE_B,
      endpoint_ref: WEBCHAT_B,
      provider: "chatgpt",
      account_ref: ACCOUNT_B,
    }],
  });

  const response = await stopThroughEdge({
    deps,
    device: DEVICE_A,
    dispatchId: runtime.dispatchId,
    idempotencyKey: "edge-runtime-harness-stop-no-grant",
  });

  assert.equal(response.body.error, undefined);
  assert.equal(transport.calls.length, before + 1, "the ungranted stop must still reach the runtime");
  const delivered = transport.calls.at(-1);
  assert.equal(delivered.frame.operation, "herdr_call");
  assert.equal(delivered.frame.workstation_id, WORKSTATION_A, "the stop must be routed to the target device's workstation");
  assert.equal(delivered.frame.trace?.webchat_control_grants, undefined, "no WebChat control grant may be forwarded for the routed device");
  assert.equal(delivered.frame.trace?.page_assist_grants, undefined, "no Page Assist grant may be forwarded for the routed device");
  assert.deepEqual(
    Object.keys(delivered.frame.trace ?? {}),
    ["webchat_authorization"],
    "only the device-independent connector authorization may ride the trace",
  );
  assert.equal(delivered.headers["x-herdr-edge-webchat-control-grants"], undefined);
  assert.equal(delivered.headers["x-herdr-edge-page-assist-grants"], undefined);
  assert.equal(
    JSON.stringify(delivered.frame).includes(WEBCHAT_B),
    false,
    "the other device's endpoint must never appear in the frame the runtime receives",
  );

  const structured = callerResultOf(response);
  assert.equal(structured.code, "caller_grant_missing", "the runtime's refusal must reach the caller unchanged");
  assert.equal(structured.actuation_available, false);
  assert.equal(structured.ok, false);
});

test("user stops only the routed device's dispatch | Given WebChat control grants on devices A and B | When the Edge forwards herdr_mcp.browser_dispatch.stop to device A | Then only A's grant reaches the runtime, which gets past its grant gate, and B's grant never appears", { skip }, async () => {
  const { runtime, transport } = await harness();
  const grantA = {
    device_id: DEVICE_A,
    endpoint_ref: runtime.endpointRef,
    provider: "chatgpt",
    account_ref: runtime.accountRef,
  };
  const grantB = {
    device_id: DEVICE_B,
    endpoint_ref: WEBCHAT_B,
    provider: "chatgpt",
    account_ref: ACCOUNT_B,
  };
  const before = transport.calls.length;
  const deps = edgeDeps({ transport, webchatControlGrants: [grantA, grantB] });

  const response = await stopThroughEdge({
    deps,
    device: DEVICE_A,
    dispatchId: runtime.dispatchId,
    idempotencyKey: "edge-runtime-harness-stop-device-a",
  });

  assert.equal(response.body.error, undefined);
  assert.equal(transport.calls.length, before + 1);
  const delivered = transport.calls.at(-1);
  assert.deepEqual(
    delivered.frame.trace?.webchat_control_grants,
    [{ endpoint_ref: runtime.endpointRef, provider: "chatgpt", account_ref: runtime.accountRef }],
    "only device A's grant may ride the forwarded trace",
  );
  const sentGrants = JSON.parse(delivered.headers["x-herdr-edge-webchat-control-grants"]);
  assert.deepEqual(
    sentGrants,
    [{ endpoint_ref: runtime.endpointRef, provider: "chatgpt", account_ref: runtime.accountRef }],
    "only device A's grant may reach the runtime's trusted grant context",
  );
  const deliveredText = JSON.stringify({ frame: delivered.frame, headers: delivered.headers });
  assert.equal(deliveredText.includes(WEBCHAT_B), false, "device B's endpoint must never reach the runtime");
  assert.equal(deliveredText.includes(ACCOUNT_B), false, "device B's account must never reach the runtime");

  // Post-grant state: the runtime accepted A's grant and moved past the grant
  // gate, then refused the stop because this dispatch never reached `applied`
  // (the harness runtime has no browser attached).
  const structured = callerResultOf(response);
  assert.equal(structured.code, "browser_dispatch_not_stoppable");
  assert.notEqual(structured.code, "caller_grant_missing");
  assert.equal(structured.actuation_available, undefined, "grant-authorized stops do not report a grant refusal");

  // Causal control: replay the exact frame the Edge forwarded, but with device
  // B's grant context. If B's grants had leaked into the forwarded call, the
  // runtime would answer differently — so this proves the assertion above is
  // decided by the grant the runtime actually received, not by fixture state.
  const leaked = await runtime.callTool(delivered.frame.operation, delivered.frame.arguments, {
    rpcId: 91,
    webchatControlGrants: [{ endpoint_ref: WEBCHAT_B, provider: "chatgpt", account_ref: ACCOUNT_B }],
  });
  assert.equal(
    leaked.headers["x-herdr-edge-webchat-control-grants"],
    JSON.stringify([{ endpoint_ref: WEBCHAT_B, provider: "chatgpt", account_ref: ACCOUNT_B }]),
  );
  assert.equal(leaked.result?.code, "caller_grant_missing", "device B's grant cannot authorize device A's endpoint");
  assert.equal(leaked.result?.actuation_available, false);
});
