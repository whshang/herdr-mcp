/**
 * Edge → runtime integration harness (issue #613).
 *
 * WHAT IS REAL HERE
 *
 *   `edge/cloudflare/dist/mcp-handler.js`        the real edge MCP handler
 *   `target/debug/herdr-mcp candidate --port N`  the real Rust runtime, built
 *                                                from this checkout, running in
 *                                                an isolated HOME/config/state
 *   grant authorization                          the real Rust grant gate
 *                                                (`caller_grant_missing`)
 *
 * WHAT IS REPLACED (transport only)
 *
 *   Edge → WorkstationDO → Relay WSS → Rust Link → local MCP HTTP
 *
 * is replaced by `createEdgeToRuntimeTransport()`. It performs the same three
 * hops the production components perform, in-process:
 *
 *   1. the WorkstationDO `forwardInternal()` projection of the Edge internal
 *      request into a Relay Protocol v1 `tool_request` frame — the frame is
 *      validated with the repo's own canonical validator, so a frame the
 *      production DO could not emit fails the test instead of being forwarded;
 *   2. the Rust Link `LocalMcpTransport` hop: `trace` grant context becomes the
 *      trusted `x-herdr-edge-*` headers and one sessionless JSON-RPC
 *      `tools/call` POST over the runtime's 0600 Unix IPC socket;
 *   3. the tool result the runtime returned is handed back to the Edge
 *      unchanged as the forwarded `completion.result`.
 *
 * The device reply is never faked: every authorization decision the tests
 * assert on is produced by the real runtime from the real request.
 *
 * SEAMS PROVIDED BY THE CALLER (not transport)
 *
 *   `resolveDevice`  the Edge device-directory lookup (same seam the existing
 *                    `edge/cloudflare/tests/*.test.mjs` suites use)
 *   `client`         the authenticated connector grant rows the Edge projects
 *
 * RUNTIME STATE PROVISIONING
 *
 * The runtime has to know the browser endpoint, provider generation, account
 * and session a routed grant authorizes. The harness provisions them through
 * the runtime's own extension registry IPC (`/extension/browser/registry`) and
 * creates the parent dispatch with a real `herdr_mcp.browser_dispatch.submit`
 * call. No state is written directly into the runtime's store and no runtime
 * behavior is stubbed: without this provisioning the grant gate for
 * `browser_dispatch.stop` is not even reached (the dispatch row is what names
 * the endpoint being authorized).
 *
 * ISOLATION
 *
 * Every runtime process gets a throwaway HOME/XDG/config/state directory, a
 * random bearer token, its own Unix IPC socket and a `HERDR_SOCKET_PATH` that
 * does not exist. The installed runtime, `~/.config/herdr-mcp`, the user CLI and
 * any network beyond 127.0.0.1 / the temp Unix socket are never touched.
 */

import { spawn } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync, mkdtempSync, rmSync, writeFileSync, mkdirSync } from "node:fs";
import http from "node:http";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

import {
  RELAY_PROTOCOL_VERSION,
  validateRelayMessage,
} from "../dist/canonical-imports.js";
import { filterWebChatGrantsForRuntime } from "../dist/webchat-grant-capability.js";
import { RUNTIME_EXECUTION_CONTRACT } from "../dist/contracts/runtime.js";

const HERE = path.dirname(fileURLToPath(import.meta.url));
/** Repository root, derived from `edge/cloudflare/tests/`. */
export const REPO_ROOT = path.resolve(HERE, "../../../");
/** Default candidate binary produced by `cargo build -p herdr-mcp --bin herdr-mcp`. */
export const DEFAULT_RUNTIME_BINARY = path.join(REPO_ROOT, "target", "debug", "herdr-mcp");
const RUNTIME_BINARY_ENV = "HERDR_MCP_EDGE_RUNTIME_BIN";
/**
 * Set to `1` (CI does) to turn "runtime binary not built" from a skip into a
 * failure, so the harness can never silently stop covering the real runtime.
 */
const REQUIRE_RUNTIME_ENV = "HERDR_MCP_EDGE_RUNTIME_REQUIRED";

/** Link hello capability of the Rust Link (`link/request_core.rs`). */
const LINK_CAPABILITIES = ["relay.request", "relay.cancel", "relay.heartbeat", "relay.status", "webchat_control.any_provider"];

const MIN_REQUEST_TIMEOUT_MS = 1_000;
const MAX_LINK_REQUEST_TIMEOUT_MS = 60_000;
const RUNTIME_READY_TIMEOUT_MS = 30_000;
const MAX_TRUSTED_GRANTS = 32;

const EDGE_WEBCHAT_CONTROL_GRANTS_HEADER = "x-herdr-edge-webchat-control-grants";
const EDGE_PAGE_ASSIST_GRANTS_HEADER = "x-herdr-edge-page-assist-grants";
const EDGE_WEBCHAT_AUTHORIZATION_HEADER = "x-herdr-edge-webchat-authorization";
const EDGE_BROWSER_CALLER_SESSION_HEADER = "x-herdr-edge-browser-caller-session";
const EDGE_CALLER_BROWSER_ON_OTHER_DEVICE_HEADER = "x-herdr-edge-caller-browser-on-other-device";
const EDGE_EXPECTED_RUNTIME_GENERATION_HEADER = "x-herdr-edge-expected-runtime-generation";

/** Resolve the candidate runtime binary for this checkout. */
export function runtimeBinaryPath() {
  const override = process.env[RUNTIME_BINARY_ENV]?.trim();
  return override ? path.resolve(override) : DEFAULT_RUNTIME_BINARY;
}

/**
 * `undefined` when the harness can run; otherwise the skip reason. Throws when a
 * required runtime is missing, so CI cannot degrade into a silent skip.
 */
export function runtimeBinarySkipReason() {
  if (process.platform === "win32") {
    // The trusted grant context only exists on the 0600 Unix IPC listener; the
    // candidate runtime refuses HERDR_EXTENSION_IPC_SOCKET on Windows.
    return "real Edge→runtime integration harness skipped: the trusted extension IPC listener is Unix-only";
  }
  const binary = runtimeBinaryPath();
  if (existsSync(binary)) return undefined;
  const reason =
    `real Edge→runtime integration harness skipped: no herdr-mcp runtime binary at ${binary}; ` +
    `build it with \`cargo build -p herdr-mcp --bin herdr-mcp\` (or point ${RUNTIME_BINARY_ENV} at one)`;
  if (process.env[REQUIRE_RUNTIME_ENV] === "1") {
    throw new Error(`${reason} — ${REQUIRE_RUNTIME_ENV}=1 requires the real runtime`);
  }
  return reason;
}

/** Every `HERDR_*`/`HERDR_MCP_*` variable is dropped: the child never inherits a runtime profile. */
function scrubbedEnvironment() {
  const env = {};
  for (const [key, value] of Object.entries(process.env)) {
    if (key.startsWith("HERDR_") || key.startsWith("HERDR_MCP_")) continue;
    if (value !== undefined) env[key] = value;
  }
  return env;
}

/** Short temp root: macOS `sockaddr_un` paths must stay under ~104 bytes. */
function tempRoot(prefix) {
  const base = process.platform === "win32" ? os.tmpdir() : "/tmp";
  return mkdtempSync(path.join(base, prefix));
}

function freeLoopbackPort() {
  return new Promise((resolve, reject) => {
    const server = net.createServer();
    server.on("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const { port } = server.address();
      server.close(() => resolve(port));
    });
  });
}

/** POST one JSON body to a Unix-socket HTTP endpoint. */
function postOverUnixSocket(socketPath, requestPath, headers, body, timeoutMs = RUNTIME_READY_TIMEOUT_MS) {
  return new Promise((resolve, reject) => {
    const payload = typeof body === "string" ? body : JSON.stringify(body);
    const request = http.request(
      {
        socketPath,
        path: requestPath,
        method: "POST",
        timeout: timeoutMs,
        headers: {
          "content-type": "application/json",
          "content-length": Buffer.byteLength(payload),
          ...headers,
        },
      },
      (response) => {
        let data = "";
        response.setEncoding("utf8");
        response.on("data", (chunk) => (data += chunk));
        response.on("end", () => resolve({ status: response.statusCode, body: data }));
      },
    );
    request.on("timeout", () => request.destroy(new Error(`runtime IPC request timed out after ${timeoutMs}ms`)));
    request.on("error", reject);
    request.end(payload);
  });
}

function getLoopback(port, requestPath, timeoutMs = 2_000) {
  return new Promise((resolve, reject) => {
    const request = http.request(
      { host: "127.0.0.1", port, path: requestPath, method: "GET", timeout: timeoutMs },
      (response) => {
        let data = "";
        response.setEncoding("utf8");
        response.on("data", (chunk) => (data += chunk));
        response.on("end", () => resolve({ status: response.statusCode, body: data }));
      },
    );
    request.on("timeout", () => request.destroy(new Error("runtime health probe timed out")));
    request.on("error", reject);
    request.end();
  });
}

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

/** Race a promise against a timer that never keeps the process alive. */
function withTimeout(promise, ms) {
  let timer;
  const timeout = new Promise((resolve) => {
    timer = setTimeout(resolve, ms);
    timer.unref?.();
  });
  return Promise.race([promise, timeout]).finally(() => clearTimeout(timer));
}

/** Stable 64-hex ref for a fixture seed, so reruns against a reused runtime stay consistent. */
function digestRef(prefix, seed) {
  return `${prefix}_${createHash("sha256").update(seed).digest("hex")}`;
}

/**
 * Normalize a `trace` grant list into the trusted header the Rust Link sends.
 * Mirrors `LocalMcpTransport::webchat_control_grants_header`, including its
 * fail-closed shape check: a malformed grant list is a link error, not a
 * silently narrowed forward.
 */
function webchatControlGrantsHeader(grants) {
  if (!Array.isArray(grants) || grants.length === 0) return undefined;
  if (grants.length > MAX_TRUSTED_GRANTS) {
    throw new Error(`the Edge forwarded ${grants.length} WebChat control grants; the Link accepts at most ${MAX_TRUSTED_GRANTS}`);
  }
  const normalized = [];
  for (const grant of grants) {
    if (!grant || typeof grant !== "object" || Array.isArray(grant)) {
      throw new Error("the Edge forwarded a non-object WebChat control grant");
    }
    const keys = Object.keys(grant);
    if (
      keys.length !== 3
      || !keys.includes("endpoint_ref")
      || !keys.includes("provider")
      || !keys.includes("account_ref")
      || typeof grant.endpoint_ref !== "string"
      || typeof grant.provider !== "string"
      || typeof grant.account_ref !== "string"
    ) {
      throw new Error(`the Edge forwarded a WebChat control grant the Link would reject: ${JSON.stringify(grant)}`);
    }
    // A wildcard endpoint is not per-endpoint authority; the Link drops it.
    if (grant.endpoint_ref === "*") continue;
    normalized.push({
      endpoint_ref: grant.endpoint_ref,
      provider: grant.provider,
      account_ref: grant.account_ref,
    });
  }
  return normalized.length > 0 ? JSON.stringify(normalized) : undefined;
}

/** Mirrors `LocalMcpTransport::page_assist_grants_header`. */
function pageAssistGrantsHeader(grants) {
  if (!Array.isArray(grants) || grants.length === 0) return undefined;
  if (grants.length > MAX_TRUSTED_GRANTS) {
    throw new Error(`the Edge forwarded ${grants.length} Page Assist grants; the Link accepts at most ${MAX_TRUSTED_GRANTS}`);
  }
  const normalized = [];
  for (const grant of grants) {
    if (!grant || typeof grant !== "object" || Array.isArray(grant)) {
      throw new Error("the Edge forwarded a non-object Page Assist grant");
    }
    const keys = Object.keys(grant);
    if (keys.length !== 1 || keys[0] !== "endpoint_ref" || typeof grant.endpoint_ref !== "string") {
      throw new Error(`the Edge forwarded a Page Assist grant the Link would reject: ${JSON.stringify(grant)}`);
    }
    if (grant.endpoint_ref === "*") continue;
    normalized.push({ endpoint_ref: grant.endpoint_ref });
  }
  return normalized.length > 0 ? JSON.stringify(normalized) : undefined;
}

/**
 * Start the real Rust runtime in an isolated profile and provision the browser
 * state a grant-routed `browser_dispatch.stop` needs.
 */
export async function startEdgeRuntime({ deviceId, profileSeed = "edge-runtime-harness-profile", generation }) {
  const binary = runtimeBinaryPath();
  if (!existsSync(binary)) throw new Error(`no herdr-mcp runtime binary at ${binary}`);

  const root = tempRoot("herdr-edge-runtime-");
  const configDir = path.join(root, "config");
  const devStateDir = path.join(root, "dev-state");
  const stateDir = path.join(root, "state");
  const xdgConfigHome = path.join(root, "xdg");
  mkdirSync(configDir, { recursive: true });
  mkdirSync(devStateDir, { recursive: true });
  mkdirSync(stateDir, { recursive: true });
  mkdirSync(xdgConfigHome, { recursive: true });
  writeFileSync(path.join(configDir, "config.json"), JSON.stringify({ edge: { device_id: deviceId } }));
  const extensionIpcSocket = path.join(root, "extension.sock");
  const port = await freeLoopbackPort();
  const runtimeGeneration = generation ?? `harness-${createHash("sha256").update(root).digest("hex").slice(0, 24)}`;

  const env = {
    ...scrubbedEnvironment(),
    HOME: root,
    XDG_CONFIG_HOME: xdgConfigHome,
    HERDR_MCP_CONFIG_DIR: configDir,
    HERDR_MCP_DEV_STATE_DIR: devStateDir,
    HERDR_MCP_STATE_DIR: stateDir,
    HERDR_MCP_TOKEN: `harness-${createHash("sha256").update(`${root}:token`).digest("hex")}`,
    HERDR_EXTENSION_IPC_SOCKET: extensionIpcSocket,
    HERDR_MCP_RUNTIME_GENERATION: runtimeGeneration,
    // Never the developer's Herdr socket: this candidate must not read or
    // mutate a real workstation's Herdr state.
    HERDR_SOCKET_PATH: path.join(root, "absent-herdr", "herdr.sock"),
  };

  const child = spawn(binary, ["candidate", "--port", String(port)], { env, stdio: ["ignore", "pipe", "pipe"] });
  let stdout = "";
  let stderr = "";
  child.stdout.setEncoding("utf8");
  child.stderr.setEncoding("utf8");
  child.stdout.on("data", (chunk) => (stdout += chunk));
  child.stderr.on("data", (chunk) => (stderr += chunk));
  let exited;
  const exitPromise = new Promise((resolve) => {
    child.on("exit", (code, signal) => {
      exited = { code, signal };
      resolve(exited);
    });
  });

  const fail = (message) => {
    child.kill("SIGKILL");
    rmSync(root, { recursive: true, force: true });
    throw new Error(`${message}\nstdout:\n${stdout}\nstderr:\n${stderr}`);
  };

  const deadline = Date.now() + RUNTIME_READY_TIMEOUT_MS;
  for (;;) {
    if (exited) fail(`herdr-mcp candidate exited before becoming ready (code=${exited.code} signal=${exited.signal})`);
    if (Date.now() > deadline) fail(`herdr-mcp candidate did not become ready within ${RUNTIME_READY_TIMEOUT_MS}ms`);
    try {
      const health = await getLoopback(port, "/health");
      if (health.status === 200 && existsSync(extensionIpcSocket)) break;
    } catch {
      // Not listening yet.
    }
    await sleep(50);
  }

  const callTool = async (name, args, context = {}) => {
    const headers = {};
    const grantsHeader = webchatControlGrantsHeader(context.webchatControlGrants);
    if (grantsHeader) headers[EDGE_WEBCHAT_CONTROL_GRANTS_HEADER] = grantsHeader;
    const pageHeader = pageAssistGrantsHeader(context.pageAssistGrants);
    if (pageHeader) headers[EDGE_PAGE_ASSIST_GRANTS_HEADER] = pageHeader;
    if (context.webchatAuthorization) {
      headers[EDGE_WEBCHAT_AUTHORIZATION_HEADER] = JSON.stringify(context.webchatAuthorization);
    }
    if (context.browserCallerSession) {
      headers[EDGE_BROWSER_CALLER_SESSION_HEADER] = JSON.stringify(context.browserCallerSession);
    }
    if (context.callerBrowserOnOtherDevice === true) {
      headers[EDGE_CALLER_BROWSER_ON_OTHER_DEVICE_HEADER] = "true";
    }
    // The runtime fences every trusted grant header on the generation the Edge
    // negotiated in the link hello. The harness is the link, so it sends the
    // generation it declared to the Edge.
    if (Object.keys(headers).length > 0) {
      headers[EDGE_EXPECTED_RUNTIME_GENERATION_HEADER] = runtimeGeneration;
    }
    const request = {
      jsonrpc: "2.0",
      id: context.rpcId ?? 1,
      method: "tools/call",
      params: { name, arguments: args },
    };
    const response = await postOverUnixSocket(extensionIpcSocket, "/mcp", headers, request);
    let json;
    try {
      json = JSON.parse(response.body);
    } catch {
      throw new Error(`runtime returned a non-JSON MCP body (status=${response.status}): ${response.body}`);
    }
    const text = json?.result?.content?.find((item) => item?.type === "text")?.text;
    let result;
    if (typeof text === "string") {
      try {
        result = JSON.parse(text);
      } catch {
        result = undefined;
      }
    }
    return { status: response.status, json, result, headers };
  };

  /** JSON-RPC discovery against the same trusted IPC socket. */
  const discover = async () => {
    const response = await postOverUnixSocket(extensionIpcSocket, "/mcp", {}, {
      jsonrpc: "2.0",
      id: 1,
      method: "server/discover",
      params: {},
    });
    try {
      return JSON.parse(response.body);
    } catch {
      throw new Error(`runtime returned a non-JSON discovery body (status=${response.status}): ${response.body}`);
    }
  };

  const registry = async (payload) => {
    const response = await postOverUnixSocket(extensionIpcSocket, "/extension/browser/registry", {}, payload);
    let json;
    try {
      json = JSON.parse(response.body);
    } catch {
      json = undefined;
    }
    if (response.status !== 200 || json?.ok !== true) {
      throw new Error(`browser registry ${payload.operation} failed (status=${response.status}): ${response.body}`);
    }
    return json;
  };

  const runtime = {
    binary,
    root,
    port,
    runtimeGeneration,
    extensionIpcSocket,
    deviceId,
    profileSeed,
    registry,
    discover,
    callTool,
    stdout: () => stdout,
    stderr: () => stderr,
    async stop() {
      if (!exited) child.kill("SIGTERM");
      await withTimeout(exitPromise, 5_000);
      if (!exited) child.kill("SIGKILL");
      await withTimeout(exitPromise, 1_000);
      rmSync(root, { recursive: true, force: true });
    },
  };

  /**
   * Provision the browser state a grant-routed `browser_dispatch.stop`
   * authorizes against, through the runtime's own extension registry IPC.
   */
  // The harness always pairs one checkout's Edge with that checkout's runtime,
  // so a contract-epoch drift between the stamped request and the runtime's
  // advertised contract means the two artifacts are not the pair under test.
  const discovery = await runtime.discover();
  const advertisedEpoch = discovery?.result?._meta?.herdr_contract_epoch;
  if (advertisedEpoch !== RUNTIME_EXECUTION_CONTRACT.contract_epoch) {
    await runtime.stop();
    throw new Error(
      `runtime advertises execution contract epoch ${advertisedEpoch} but this edge stamps ${RUNTIME_EXECUTION_CONTRACT.contract_epoch}`,
    );
  }

  const provisionBrowserState = async () => {
    const endpoint = await runtime.registry({
      operation: "endpoint.register",
      profile_seed: profileSeed,
      browser_family: "chrome",
      extension_version: "0.1.0",
    });
    const endpointRef = endpoint.endpoint.endpoint_ref;
    await runtime.registry({
      operation: "endpoint.consent",
      profile_seed: profileSeed,
      endpoint_ref: endpointRef,
      expected_revision: 0,
      webchat_control_allowed: true,
      tool_bridge_allowed: false,
      tool_bridge_mutation_allowed: false,
    });
    const observationGeneration = 7;
    await runtime.registry({
      operation: "provider.observe",
      profile_seed: profileSeed,
      endpoint_ref: endpointRef,
      provider: "chatgpt",
      adapter_protocol_version: 1,
      observation_generation: observationGeneration,
      capabilities: { operations: ["composer.submit", "generation.stop"] },
    });
    const account = await runtime.registry({
      operation: "resource.observe",
      profile_seed: profileSeed,
      endpoint_ref: endpointRef,
      provider: "chatgpt",
      kind: "account",
      native_identity: `${profileSeed}:account`,
      observation_generation: observationGeneration,
    });
    const accountRef = account.resource.resource_ref;
    const session = await runtime.registry({
      operation: "resource.observe",
      profile_seed: profileSeed,
      endpoint_ref: endpointRef,
      provider: "chatgpt",
      kind: "session",
      parent_ref: accountRef,
      native_identity: `${profileSeed}:session`,
      canonical_url: "https://chatgpt.com/c/edge-runtime-harness",
      observation_generation: observationGeneration,
    });
    const sessionRef = session.resource.resource_ref;
    const grant = {
      endpoint_ref: endpointRef,
      provider: "chatgpt",
      account_ref: accountRef,
    };

    // A real submit reserves the parent dispatch the routed stop authorizes
    // against. No browser extension is attached to this runtime, so the submit
    // settles as `browser_offline`; the durable dispatch row is what makes the
    // stop's grant gate run at all.
    const submit = await runtime.callTool(
      "herdr_call",
      {
        method: "herdr_mcp.browser_dispatch.submit",
        params: JSON.stringify({
          session_ref: sessionRef,
          message: "harness submit that the routed stop must authorize against",
          expected_generation: observationGeneration,
          idempotency_key: digestRef("harness-submit", profileSeed),
        }),
      },
      { webchatControlGrants: [grant] },
    );
    const dispatchId = submit.result?.dispatch?.dispatch_id;
    if (typeof dispatchId !== "string" || !dispatchId.startsWith("bd_")) {
      throw new Error(`harness could not provision a parent dispatch: ${JSON.stringify(submit.json)}`);
    }
    return { observationGeneration, endpointRef, accountRef, sessionRef, dispatchId, grant, submitResult: submit.result };
  };

  try {
    return { ...runtime, ...(await provisionBrowserState()) };
  } catch (error) {
    await runtime.stop();
    throw error;
  }
}

/**
 * Transport adapter replacing only the Edge→device hops for one runtime.
 *
 * `forward(stub, body)` receives exactly what the production handler hands the
 * WorkstationDO stub, emits a validated Relay Protocol v1 `tool_request`, sends
 * it through the trusted-IPC hop the Rust Link performs, and returns the
 * runtime's own tool result as the forwarded completion.
 */
export function createEdgeToRuntimeTransport(runtime, { workstationId, linkCapabilities = LINK_CAPABILITIES } = {}) {
  const calls = [];
  let rpcId = 0;

  const forward = async (stub, body) => {
    const internal = JSON.parse(body);
    if (internal?.kind !== "request") {
      throw new Error(`edge forwarded a non-request internal body: ${body}`);
    }
    const trustedTrace = filterWebChatGrantsForRuntime(internal.trace, linkCapabilities);
    const deadlineMs = Number.isFinite(internal.deadlineMs) ? internal.deadlineMs : Date.now() + MAX_LINK_REQUEST_TIMEOUT_MS;
    const frame = JSON.parse(JSON.stringify({
      protocol_version: RELAY_PROTOCOL_VERSION,
      kind: "tool_request",
      workstation_id: stub?.workstationId ?? workstationId,
      request_id: internal.requestId,
      operation: internal.op,
      timeout_ms: Math.min(MAX_LINK_REQUEST_TIMEOUT_MS, Math.max(MIN_REQUEST_TIMEOUT_MS, deadlineMs - Date.now())),
      contract_epoch: internal.contractEpoch,
      contract_hash: internal.contractHash,
      idempotency_key: internal.idempotencyKey,
      arguments: internal.args,
      trace: trustedTrace,
    }));
    const check = validateRelayMessage(frame, { strictUnknownFields: true });
    if (!check.ok) {
      throw new Error(`edge emitted a relay tool_request the production DO would reject: ${check.code} ${check.reason}`);
    }

    // Rust Link `LocalMcpTransport` hop: trace → trusted headers → one JSON-RPC POST.
    const delivered = await runtime.callTool(frame.operation, frame.arguments ?? {}, {
      rpcId: ++rpcId,
      webchatControlGrants: frame.trace?.webchat_control_grants,
      pageAssistGrants: frame.trace?.page_assist_grants,
      webchatAuthorization: frame.trace?.webchat_authorization,
      browserCallerSession: frame.trace?.browser_caller_session,
      callerBrowserOnOtherDevice: frame.trace?.caller_browser_on_other_device,
    });
    calls.push({ frame, headers: delivered.headers, status: delivered.status, rpc: delivered.json });

    if (delivered.json?.error) {
      const envelope = {
        status: "ok",
        completion: {
          status: "error",
          error: {
            code: "runtime_tool_error",
            requestId: frame.request_id,
            workstationId: frame.workstation_id,
            message: delivered.json.error.message ?? "runtime tool error",
          },
        },
      };
      return new Response(JSON.stringify(envelope), { headers: { "content-type": "application/json" } });
    }
    if (delivered.status !== 200 || !delivered.json?.result) {
      return new Response(JSON.stringify({
        status: "error",
        error: {
          code: "runtime_unavailable",
          requestId: frame.request_id,
          workstationId: frame.workstation_id,
          retryable: true,
          delivery_state: "not_delivered",
        },
      }), { headers: { "content-type": "application/json" } });
    }
    const envelope = {
      status: "ok",
      completion: { status: "ok", result: delivered.json.result },
    };
    return new Response(JSON.stringify(envelope), { headers: { "content-type": "application/json" } });
  };

  return { forward, calls };
}

/**
 * Handler dependencies with the real Edge handler's transport and the Edge's
 * device directory / connector grant seams supplied by the caller.
 */
export function createEdgeDeps({ transport, client, resolveDevice, devices = [], edgeEnv, limits, now }) {
  const targets = [];
  const logs = [];
  return {
    targets,
    logs,
    value: {
      limits,
      edgeEnv,
      logger: {
        warn(event, fields) {
          logs.push({ event, fields });
        },
      },
      getStub: (workstationId) => {
        targets.push(workstationId);
        return { workstationId };
      },
      listDevices: async () => devices,
      client,
      resolveDevice,
      forward: transport.forward,
      now: now ?? (() => Date.now()),
    },
  };
}

/**
 * The result object the caller receives from `handleMcp`.
 *
 * A real runtime tool result reaches the caller as the model-visible MCP
 * `content[0].text` JSON (the runtime does not emit `structuredContent`), so this
 * prefers `structuredContent` when present and otherwise parses that text.
 */
export function callerResultOf(response) {
  const result = response?.body?.result;
  if (result && typeof result === "object" && result.structuredContent && typeof result.structuredContent === "object") {
    return result.structuredContent;
  }
  const text = Array.isArray(result?.content)
    ? result.content.find((item) => item?.type === "text")?.text
    : undefined;
  if (typeof text !== "string") return undefined;
  try {
    return JSON.parse(text);
  } catch {
    return undefined;
  }
}
