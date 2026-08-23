import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, readFile, stat, writeFile, mkdir } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, dirname } from "node:path";
import { apply, preflight, rollback, CONTRACT_HASH } from "../bin/herdr-edge-cutover";

function config(statePath, overrides = {}) {
  return {
    apiBase: "https://cf.test/client/v4",
    token: "SECRET-CF-TOKEN",
    zoneName: "agentforme.cc.cd",
    zoneId: "",
    pattern: "herdr-mcp.agentforme.cc.cd/*",
    worker: "herdr-edge-prod",
    publicBase: "https://public.test",
    prodEdgeBase: "https://prod.test",
    workstationId: "prod-real-runtime",
    probeBearer: "SECRET-PROBE-TOKEN",
    statePath,
    watchAttempts: 2,
    watchFailures: 2,
    watchIntervalMs: 0,
    ...overrides,
  };
}

function json(body, status = 200) {
  return new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
}

function tools() {
  return Array.from({ length: 17 }, (_, i) => ({ name: `tool_${i}` }));
}

function goodMcp(id = "cutover-probe") {
  return json({ jsonrpc: "2.0", id, result: { tools: tools(), _meta: { herdr: { contract_hash: CONTRACT_HASH } } } });
}

function goodLegacyDiscover() {
  return json({ jsonrpc: "2.0", id: "legacy-probe", result: { _meta: { "io.modelcontextprotocol/serverInfo": { name: "herdr-mcp", version: "0.3.23" } } } });
}

async function tempState() {
  const dir = await mkdtemp(join(tmpdir(), "herdr-cutover-test-"));
  return join(dir, "state.json");
}

async function withFetch(handler, fn) {
  const original = globalThis.fetch;
  globalThis.fetch = handler;
  try { return await fn(); } finally { globalThis.fetch = original; }
}

function readyHandler({ routes = [], publicMode = "legacy", onCreate, onDelete } = {}) {
  return async (input, init = {}) => {
    const u = new URL(typeof input === "string" ? input : input.url);
    const method = (init.method || "GET").toUpperCase();
    if (u.hostname === "cf.test") {
      if (u.pathname === "/client/v4/zones") return json({ success: true, result: [{ id: "zone1" }] });
      if (u.pathname === "/client/v4/zones/zone1/workers/routes" && method === "GET") return json({ success: true, result: routes });
      if (u.pathname === "/client/v4/zones/zone1/workers/routes" && method === "POST") {
        onCreate?.(JSON.parse(init.body));
        routes.splice(0, routes.length, { id: "route1", pattern: "herdr-mcp.agentforme.cc.cd/*", script: "herdr-edge-prod" });
        return json({ success: true, result: routes[0] });
      }
      if (u.pathname === "/client/v4/zones/zone1/workers/routes/route1" && method === "DELETE") {
        onDelete?.();
        routes.splice(0, routes.length);
        return json({ success: true, result: { id: "route1" } });
      }
    }
    if (u.hostname === "prod.test") {
      if (u.pathname === "/health") return json({ ok: true, contractEpoch: 1, contractHash: CONTRACT_HASH });
      if (u.pathname === "/status/prod-real-runtime") return json({ ok: true, online: true, runtimeVersion: "0.3.23", contractEpoch: 1, contractHash: CONTRACT_HASH });
      if (u.pathname === "/mcp") return goodMcp();
    }
    if (u.hostname === "public.test") {
      if (u.pathname === "/health") {
        if (publicMode === "edge") return json({ ok: true, contractEpoch: 1, contractHash: CONTRACT_HASH });
        if (publicMode === "fail") return json({ ok: false }, 503);
        return json({ ok: false }, 404);
      }
      if (u.pathname === "/mcp") {
        if (publicMode === "edge") return goodMcp();
        if (publicMode === "fail") return json({ ok: false }, 503);
        return goodLegacyDiscover();
      }
    }
    return json({ error: "unexpected", url: u.href, method }, 500);
  };
}

test("preflight reports permission_blocked on Workers Routes 403", async () => {
  const state = await tempState();
  const c = config(state);
  await withFetch(async (input) => {
    const u = new URL(typeof input === "string" ? input : input.url);
    if (u.pathname === "/client/v4/zones") return json({ success: true, result: [{ id: "zone1" }] });
    if (u.pathname.endsWith("/workers/routes")) return json({ success: false, errors: [{ code: 10000 }] }, 403);
    return json({}, 500);
  }, async () => {
    const r = await preflight(c);
    assert.equal(r.ok, false);
    assert.equal(r.code, "permission_blocked");
  });
});

test("preflight can use a pre-resolved zone id without Zone Read", async () => {
  const c = config(await tempState(), { zoneId: "zone1" });
  let zoneLookupCalls = 0;
  const base = readyHandler();
  await withFetch(async (input, init) => {
    const u = new URL(typeof input === "string" ? input : input.url);
    if (u.pathname === "/client/v4/zones") { zoneLookupCalls += 1; return json({ success: false }, 403); }
    return base(input, init);
  }, async () => {
    const r = await preflight(c);
    assert.equal(r.ok, true);
    assert.equal(zoneLookupCalls, 0);
  });
});

test("preflight passes with no exact route, warm prod Edge, and legacy origin", async () => {
  const c = config(await tempState());
  await withFetch(readyHandler(), async () => {
    const r = await preflight(c);
    assert.equal(r.ok, true);
    assert.equal(r.prod.online, true);
    assert.equal(r.legacy.version, "0.3.23");
  });
});

test("preflight refuses a conflicting exact route", async () => {
  const c = config(await tempState());
  const routes = [{ id: "other", pattern: c.pattern, script: "other-worker" }];
  await withFetch(readyHandler({ routes }), async () => {
    const r = await preflight(c);
    assert.equal(r.ok, false);
    assert.equal(r.code, "route_conflict");
  });
});

test("apply creates exact route, watches healthy public Edge, and writes mode-600 secret-free state", async () => {
  const statePath = await tempState();
  const c = config(statePath);
  const routes = [];
  let created = null;
  const base = readyHandler({ routes, onCreate: (x) => { created = x; } });
  await withFetch(async (input, init) => {
    const u = new URL(typeof input === "string" ? input : input.url);
    // Before route create public host is legacy; after create it is Edge.
    if (u.hostname === "public.test") {
      if (routes.length === 0) return u.pathname === "/mcp" ? goodLegacyDiscover() : json({}, 404);
      if (u.pathname === "/health") return json({ ok: true, contractEpoch: 1, contractHash: CONTRACT_HASH });
      if (u.pathname === "/mcp") return goodMcp();
    }
    return base(input, init);
  }, async () => {
    const r = await apply(c);
    assert.equal(r.ok, true);
    assert.deepEqual(created, { pattern: c.pattern, script: c.worker });
    const saved = await readFile(statePath, "utf8");
    assert.equal(saved.includes(c.token), false);
    assert.equal(saved.includes(c.probeBearer), false);
    assert.equal(JSON.parse(saved).status, "observed_healthy");
    assert.equal((await stat(statePath)).mode & 0o777, 0o600);
  });
});

test("apply recovers an ambiguous POST when route was committed before response loss", async () => {
  const statePath = await tempState();
  const c = config(statePath, { watchAttempts: 1, watchFailures: 1 });
  const routes = [];
  const base = readyHandler({ routes });
  let lostOnce = false;
  await withFetch(async (input, init = {}) => {
    const u = new URL(typeof input === "string" ? input : input.url);
    const method = (init.method || "GET").toUpperCase();
    if (u.hostname === "cf.test" && u.pathname === "/client/v4/zones/zone1/workers/routes" && method === "POST" && !lostOnce) {
      lostOnce = true;
      routes.push({ id: "route1", pattern: c.pattern, script: c.worker });
      throw new Error("response lost after commit");
    }
    if (u.hostname === "public.test") {
      if (routes.length === 0) return u.pathname === "/mcp" ? goodLegacyDiscover() : json({}, 404);
      if (u.pathname === "/health") return json({ ok: true, contractEpoch: 1, contractHash: CONTRACT_HASH });
      if (u.pathname === "/mcp") return goodMcp();
    }
    return base(input, init);
  }, async () => {
    const r = await apply(c);
    assert.equal(r.ok, true);
    assert.equal(lostOnce, true);
    const saved = JSON.parse(await readFile(statePath, "utf8"));
    assert.equal(saved.routeId, "route1");
    assert.equal(saved.status, "observed_healthy");
  });
});

test("watch failures automatically delete only the created exact route", async () => {
  const statePath = await tempState();
  const c = config(statePath, { watchAttempts: 3, watchFailures: 2 });
  const routes = [];
  let deleted = 0;
  const base = readyHandler({ routes, onDelete: () => { deleted++; } });
  await withFetch(async (input, init) => {
    const u = new URL(typeof input === "string" ? input : input.url);
    if (u.hostname === "public.test") {
      if (routes.length === 0) return u.pathname === "/mcp" ? goodLegacyDiscover() : json({}, 404);
      return json({ ok: false }, 503);
    }
    return base(input, init);
  }, async () => {
    const r = await apply(c);
    assert.equal(r.ok, false);
    assert.equal(r.code, "watch_failed_rolled_back");
    assert.equal(r.rollback.ok, true);
    assert.equal(deleted, 1);
    assert.equal(JSON.parse(await readFile(statePath, "utf8")).status, "rolled_back");
  });
});

test("rollback refuses route id whose current pattern/script no longer match state", async () => {
  const statePath = await tempState();
  const c = config(statePath);
  await mkdir(dirname(statePath), { recursive: true });
  await writeFile(statePath, JSON.stringify({ routeId: "route1", pattern: c.pattern, worker: c.worker }), { mode: 0o600 });
  const routes = [{ id: "route1", pattern: c.pattern, script: "attacker-or-other-worker" }];
  await withFetch(readyHandler({ routes }), async () => {
    const r = await rollback(c);
    assert.equal(r.ok, false);
    assert.equal(r.code, "route_identity_mismatch");
    assert.equal(routes.length, 1);
  });
});
