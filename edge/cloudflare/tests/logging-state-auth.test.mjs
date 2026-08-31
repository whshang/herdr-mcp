import { test } from "node:test";
import assert from "node:assert/strict";
import {
  createLogger,
  sanitize,
} from "../dist/logger.js";
import {
  sessionFromClaims,
  parseSession,
  serializeSession,
  sessionSummary,
  applyRuntimeStatusGlimpse,
  isStale,
  makeEmptySession,
} from "../dist/state.js";
import { makeLimits } from "../dist/limits.js";
import {
  SharedSecretLinkAuthenticator,
  authenticateStaticMcpBearer,
  buildLinkAuthProtocol,
  extractLinkCredential,
  hasLinkApplicationProtocol,
} from "../dist/auth.js";
import { EPOCH2_CONTRACT } from "../dist/contracts/epoch2.js";

function capturedLogger(scope) {
  const lines = [];
  const sink = { write: (line) => lines.push(JSON.parse(line)) };
  return { logger: createLogger(scope, { sink }), lines };
}

test("logger: never emits sensitive or body-shaped fields", () => {
  const { logger, lines } = capturedLogger("t");
  logger.info("req", {
    requestId: "r1",
    workstationId: "w1",
    authorization: "Bearer SECRET-TOKEN",
    args: { cmd: "rm -rf /" },
    prompt: "delete everything",
    tool: "herdr_exec",
    status: 200,
  });
  assert.equal(lines.length, 1);
  const fields = lines[0].fields;
  assert.equal(fields.authorization, "[redacted]");
  assert.equal(fields.args, "[omitted]");
  assert.equal(fields.prompt, "[omitted]");
  assert.equal(fields.tool, "herdr_exec");
  assert.equal(fields.requestId, "r1");
  assert.equal(JSON.stringify(lines).includes("SECRET-TOKEN"), false);
  assert.equal(JSON.stringify(lines).includes("rm -rf"), false);
});

test("logger: sanitize redacts token-like keys", () => {
  const out = sanitize({ apiKey: "abc", fine: 1, nested: { secret: "s" } });
  assert.equal(out.apiKey, "[redacted]");
  assert.equal(out.nested.secret, "[redacted]");
  assert.equal(out.fine, 1);
});

test("state: hello claims round-trip through storage blob", () => {
  const session = sessionFromClaims({
    workstationId: "w1",
    linkVersion: "0.1.0",
    bootId: "boot1",
    protocolVersion: "1",
    connectedAtMs: 1000,
    runtimeVersion: "0.3.32",
    runtimeGeneration: "g1",
    contractHash: EPOCH2_CONTRACT.contract_hash,
    contractEpoch: EPOCH2_CONTRACT.contract_epoch,
    capabilities: ["herdr", "fs"],
  });
  const raw = serializeSession(session);
  const parsed = parseSession(raw);
  assert.equal(parsed.ok, true);
  assert.equal(parsed.session.workstationId, "w1");
  assert.equal(parsed.session.hello.bootId, "boot1");
  assert.equal(parsed.session.status, "online");
  assert.equal(parsed.session.hello.protocolVersion, "1");
  assert.equal(parsed.session.hello.runtimeVersion, "0.3.32");
  assert.equal(parsed.session.hello.runtimeGeneration, "g1");
  assert.equal(parsed.session.hello.contractHash, EPOCH2_CONTRACT.contract_hash);
  assert.equal(parsed.session.hello.contractEpoch, EPOCH2_CONTRACT.contract_epoch);
});

test("state: heartbeat runtime glimpse overrides stale hello identity and survives persistence", () => {
  const session = sessionFromClaims({
    workstationId: "w1",
    linkVersion: "0.1.0",
    bootId: "boot1",
    protocolVersion: "1",
    connectedAtMs: 1000,
    runtimeVersion: "0.3.23",
    runtimeGeneration: "stable-023",
  });
  assert.equal(applyRuntimeStatusGlimpse(session, {
    runtime_version: "0.3.26",
    runtime_generation: "candidate-026",
    herdr_protocol: "20",
  }, true), true);
  assert.equal(applyRuntimeStatusGlimpse(session, {
    runtime_version: "0.3.26",
    runtime_generation: "candidate-026",
    herdr_protocol: "20",
  }, true), false, "identical heartbeat must not force another persistence write");
  const parsed = parseSession(serializeSession(session));
  assert.equal(parsed.ok, true);
  const summary = sessionSummary(parsed.session, { now: 1100, linkStaleAfterMs: 5000, activeRequests: 0, edgeVersion: "edge" });
  assert.equal(summary.runtimeVersion, "0.3.26");
  assert.equal(summary.runtimeGeneration, "candidate-026");
  assert.equal(summary.herdProtocolVersion, "20");
  assert.equal(summary.runtimeHealth, "ok");
});

test("state: parseSession rejects malformed", () => {
  assert.equal(parseSession("nope").ok, false);
  assert.equal(parseSession(JSON.stringify({ kind: "wrong" })).ok, false);
  assert.equal(
    parseSession(JSON.stringify({ kind: "herdr-edge/workstation-session", schemaVersion: 1, session: { workstationId: "" } })).ok,
    false,
  );
});

test("state: sessionSummary reflects stale link", () => {
  const limits = makeLimits({ LINK_STALE_AFTER_MS: "5000" });
  const session = sessionFromClaims({
    workstationId: "w1", linkVersion: "0.1.0", bootId: "b", protocolVersion: "1", connectedAtMs: 0,
  });
  assert.equal(isStale(limits, session, 10_000), true);
  assert.equal(isStale(limits, session, 1000), false);
  const summary = sessionSummary(session, { now: 10_000, linkStaleAfterMs: 5000, activeRequests: 2, edgeVersion: "0.1.0-dev" });
  assert.equal(summary.online, false);
  assert.equal(summary.activeRequests, 2);
  assert.equal(summary.runtimeVersion, undefined);
});

test("state: status falls back to hello runtime/contract identity across hibernation", () => {
  const session = sessionFromClaims({
    workstationId: "w1",
    linkVersion: "0.1.0",
    bootId: "boot1",
    protocolVersion: "1",
    connectedAtMs: 1000,
    runtimeVersion: "0.3.32",
    runtimeCommit: "dev",
    runtimeGeneration: "live-0.3.32",
    herdProtocolVersion: "20",
    contractHash: EPOCH2_CONTRACT.contract_hash,
    contractEpoch: EPOCH2_CONTRACT.contract_epoch,
  });
  const parsed = parseSession(serializeSession(session));
  assert.equal(parsed.ok, true);
  const summary = sessionSummary(parsed.session, { now: 1100, linkStaleAfterMs: 5000, activeRequests: 0, edgeVersion: "0.1.0-dev" });
  assert.equal(summary.runtimeVersion, "0.3.32");
  assert.equal(summary.runtimeCommit, "dev");
  assert.equal(summary.runtimeGeneration, "live-0.3.32");
  assert.equal(summary.herdProtocolVersion, "20");
  assert.equal(summary.contractEpoch, EPOCH2_CONTRACT.contract_epoch);
  assert.equal(summary.contractHash, EPOCH2_CONTRACT.contract_hash);
});

test("state: makeEmptySession starts offline", () => {
  const s = makeEmptySession("w2", 1234);
  assert.equal(s.status, "offline");
  assert.equal(s.workstationId, "w2");
});

test("auth: shared-secret link authenticator matches and rejects", () => {
  const auth = new SharedSecretLinkAuthenticator({ secret: "dev-secret" });
  const okReq = { headers: { get: (name) => name.toLowerCase() === "authorization" ? "Bearer dev-secret" : null } };
  assert.equal(auth.authenticate(okReq, "w1", 1).ok, true);
  const subprotocolReq = {
    headers: {
      get: (name) => name.toLowerCase() === "sec-websocket-protocol"
        ? `herdr-link.v1, ${buildLinkAuthProtocol("dev-secret")}`
        : null,
    },
  };
  assert.equal(hasLinkApplicationProtocol(subprotocolReq), true);
  assert.equal(auth.authenticate(subprotocolReq, "w1", 1).ok, true);
  const authOnlyReq = {
    headers: {
      get: (name) => name.toLowerCase() === "sec-websocket-protocol"
        ? buildLinkAuthProtocol("dev-secret")
        : null,
    },
  };
  assert.equal(hasLinkApplicationProtocol(authOnlyReq), false);
  const badReq = { headers: { get: (name) => name.toLowerCase() === "authorization" ? "Bearer wrong" : null } };
  assert.equal(auth.authenticate(badReq, "w1", 1).ok, false);
  const noneReq = { headers: { get: () => null } };
  assert.equal(auth.authenticate(noneReq, "w1", 1).ok, false);
});

test("auth: fails closed when secret unset", () => {
  const auth = new SharedSecretLinkAuthenticator({});
  const req = { headers: { get: () => "Bearer x" } };
  assert.equal(auth.authenticate(req, "w1", 1).ok, false);
});

test("auth: credential extraction accepts exactly one bounded bearer or websocket secret", () => {
  const bearer = { headers: { get: (name) => name.toLowerCase() === "authorization" ? "Bearer device-secret" : null } };
  assert.deepEqual(extractLinkCredential(bearer), {
    ok: true,
    credential: "device-secret",
    transport: "authorization",
  });
  const websocket = {
    headers: {
      get: (name) => name.toLowerCase() === "sec-websocket-protocol"
        ? `herdr-link.v1, ${buildLinkAuthProtocol("device-secret")}`
        : null,
    },
  };
  assert.deepEqual(extractLinkCredential(websocket), {
    ok: true,
    credential: "device-secret",
    transport: "websocket_protocol",
  });
  const multiple = {
    headers: {
      get: (name) => name.toLowerCase() === "sec-websocket-protocol"
        ? `${buildLinkAuthProtocol("a")}, ${buildLinkAuthProtocol("b")}`
        : null,
    },
  };
  assert.equal(extractLinkCredential(multiple).ok, false);
});

test("auth: static MCP bearer is separate and fail-closed", () => {
  const bearer = (value) => ({
    headers: { get: (name) => name.toLowerCase() === "authorization" ? value : null },
  });
  assert.equal(authenticateStaticMcpBearer(bearer("Bearer mcp-secret"), "mcp-secret").ok, true);
  assert.equal(authenticateStaticMcpBearer(bearer("Bearer wrong"), "mcp-secret").ok, false);
  assert.equal(authenticateStaticMcpBearer(bearer("Bearer mcp-secret"), undefined).ok, false);
  assert.equal(authenticateStaticMcpBearer({ headers: { get: () => null } }, "mcp-secret").ok, false);
});