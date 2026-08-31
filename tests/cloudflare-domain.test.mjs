import assert from "node:assert/strict";
import { mkdtemp, readFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

import {
  attach, config, detach, preflight, status, watch,
  CONTRACT_EPOCH, CONTRACT_HASH, REQUIRED_TOOL_COUNT, RUNTIME_VERSION,
} from "../bin/herdr-cloudflare-domain";

function json(body, statusCode = 200) {
  return new Response(JSON.stringify(body), { status: statusCode, headers: { "content-type": "application/json" } });
}

function tools() {
  return [{ name: "herdr_skill" }, ...Array.from({ length: REQUIRED_TOOL_COUNT - 1 }, (_, i) => ({ name: `tool_${i}` }))];
}

async function statePath() {
  const dir = await mkdtemp(join(tmpdir(), "herdr-domain-test-"));
  return join(dir, "state.json");
}

function cfg(overrides = {}) {
  return config({
    CLOUDFLARE_API_TOKEN: "cf-secret",
    CLOUDFLARE_ACCOUNT_ID: "account1",
    HERDR_CUTOVER_ZONE_ID: "zone1",
    HERDR_CUTOVER_ZONE: "example.com",
    HERDR_CUSTOM_DOMAIN: "herdr.example.com",
    HERDR_CUTOVER_WORKER: "herdr-edge-prod",
    HERDR_CUTOVER_PROD_EDGE: "https://prod.test",
    HERDR_CUTOVER_WORKSTATION: "prod-real-runtime",
    HERDR_CUTOVER_PROBE_BEARER: "probe-secret",
    HERDR_CUTOVER_WATCH_ATTEMPTS: "2",
    HERDR_CUTOVER_FAILURE_THRESHOLD: "2",
    HERDR_CUTOVER_WATCH_INTERVAL_MS: "0",
    ...overrides,
  });
}

function handler({ domains = [], publicHealthy = true, publicToolCount = REQUIRED_TOOL_COUNT, onAttach, onDetach } = {}) {
  return async (input, init = {}) => {
    const u = new URL(typeof input === "string" ? input : input.url);
    const method = (init.method || "GET").toUpperCase();
    if (u.hostname === "cf.test" || u.hostname === "api.cloudflare.com") {
      if (u.pathname === "/client/v4/accounts/account1/workers/domains" && method === "GET") {
        return json({ success: true, result: domains });
      }
      if (u.pathname === "/client/v4/accounts/account1/workers/domains" && method === "PUT") {
        const body = JSON.parse(init.body);
        onAttach?.(body);
        const domain = { id: "domain1", hostname: body.hostname, service: body.service, zone_id: body.zone_id, zone_name: body.zone_name };
        domains.splice(0, domains.length, domain);
        return json({ success: true, result: domain });
      }
      if (u.pathname === "/client/v4/accounts/account1/workers/domains/domain1" && method === "DELETE") {
        onDetach?.();
        domains.splice(0, domains.length);
        return json({ success: true, result: null });
      }
    }
    if (u.hostname === "prod.test" || u.hostname === "herdr.example.com") {
      const healthy = u.hostname === "prod.test" || publicHealthy;
      if (u.pathname === "/health") return healthy ? json({
        ok: true,
        contractEpoch: 3,
        contractHash: "sha256:public-v3",
        runtimeContractEpoch: CONTRACT_EPOCH,
        runtimeContractHash: CONTRACT_HASH,
      }) : json({ ok: false }, 503);
      if (u.pathname === "/status/prod-real-runtime") return healthy ? json({ ok: true, online: true, runtimeVersion: RUNTIME_VERSION, contractEpoch: CONTRACT_EPOCH, contractHash: CONTRACT_HASH }) : json({ ok: false }, 503);
      if (u.pathname === "/mcp") return healthy ? json({ jsonrpc: "2.0", id: "domain-probe", result: { tools: [{ name: "herdr_skill" }, ...Array.from({ length: publicToolCount - 1 }, (_, i) => ({ name: `tool_${i}` }))], _meta: { herdr: { contract_hash: "sha256:public-v3" } } } }) : json({ ok: false }, 503);
    }
    return json({ error: "unexpected", url: u.href, method }, 500);
  };
}

async function withFetch(fetchImpl, fn) {
  const old = globalThis.fetch;
  globalThis.fetch = fetchImpl;
  try { return await fn(); } finally { globalThis.fetch = old; }
}

test("preflight validates workers.dev candidate and reports ready_to_attach", async () => {
  const c = cfg({ CF_API_BASE: "https://cf.test/client/v4", HERDR_CUSTOM_DOMAIN_STATE_PATH: await statePath() });
  await withFetch(handler(), async () => {
    const r = await preflight(c);
    assert.equal(r.ok, true);
    assert.equal(r.code, "ready_to_attach");
    assert.equal(r.prod.online, true);
    assert.equal(r.prod.toolCount, REQUIRED_TOOL_COUNT);
    assert.equal(r.prod.contractEpoch, CONTRACT_EPOCH);
    assert.equal(r.prod.publicContractEpoch, 3);
  });
});

test("preflight accepts a public tool surface larger than the runtime execution contract", async () => {
  const c = cfg({ CF_API_BASE: "https://cf.test/client/v4", HERDR_CUSTOM_DOMAIN_STATE_PATH: await statePath() });
  await withFetch(handler({ publicToolCount: REQUIRED_TOOL_COUNT + 1 }), async () => {
    const r = await preflight(c);
    assert.equal(r.ok, true);
    assert.equal(r.prod.toolCount, REQUIRED_TOOL_COUNT + 1);
    assert.equal(r.prod.contractEpoch, CONTRACT_EPOCH);
  });
});

test("preflight refuses a custom domain owned by another worker", async () => {
  const domains = [{ id: "d", hostname: "herdr.example.com", service: "other-worker" }];
  const c = cfg({ CF_API_BASE: "https://cf.test/client/v4", HERDR_CUSTOM_DOMAIN_STATE_PATH: await statePath() });
  await withFetch(handler({ domains }), async () => {
    const r = await preflight(c);
    assert.equal(r.ok, false);
    assert.equal(r.code, "domain_conflict");
  });
});

test("attach uses Workers Domains API and never edits DNS", async () => {
  const domains = [];
  let body = null;
  const c = cfg({ CF_API_BASE: "https://cf.test/client/v4", HERDR_CUSTOM_DOMAIN_STATE_PATH: await statePath() });
  await withFetch(handler({ domains, onAttach: (x) => { body = x; } }), async () => {
    const r = await attach(c);
    assert.equal(r.ok, true);
    assert.equal(r.code, "domain_attached");
    assert.deepEqual(body, { hostname: "herdr.example.com", service: "herdr-edge-prod", zone_id: "zone1", zone_name: "example.com" });
  });
});

test("attach recovers when PUT committed but response was lost", async () => {
  const domains = [];
  const c = cfg({ CF_API_BASE: "https://cf.test/client/v4", HERDR_CUSTOM_DOMAIN_STATE_PATH: await statePath() });
  const base = handler({ domains });
  let putCalls = 0;
  await withFetch(async (input, init = {}) => {
    const u = new URL(typeof input === "string" ? input : input.url);
    if (u.hostname === "cf.test" && u.pathname === "/client/v4/accounts/account1/workers/domains" && (init.method || "GET").toUpperCase() === "PUT") {
      putCalls += 1;
      const body = JSON.parse(init.body);
      domains.splice(0, domains.length, { id: "domain1", hostname: body.hostname, service: body.service });
      throw new TypeError("response lost after commit");
    }
    return base(input, init);
  }, async () => {
    const r = await attach(c);
    assert.equal(r.ok, true);
    assert.equal(r.recovered, true);
    assert.equal(putCalls, 1);
  });
});

test("watch verifies the custom hostname after attach", async () => {
  const c = cfg({ CF_API_BASE: "https://cf.test/client/v4", HERDR_CUSTOM_DOMAIN_STATE_PATH: await statePath() });
  await withFetch(handler({ publicHealthy: true }), async () => {
    const r = await watch(c);
    assert.equal(r.ok, true);
    assert.equal(r.code, "custom_domain_healthy");
  });
});

test("detach refuses mismatched worker identity and deletes only expected domain", async () => {
  const c = cfg({ CF_API_BASE: "https://cf.test/client/v4", HERDR_CUSTOM_DOMAIN_STATE_PATH: await statePath() });
  await withFetch(handler({ domains: [{ id: "x", hostname: "herdr.example.com", service: "other-worker" }] }), async () => {
    const bad = await detach(c);
    assert.equal(bad.ok, false);
    assert.equal(bad.code, "domain_identity_mismatch");
  });
  let deleted = false;
  await withFetch(handler({ domains: [{ id: "domain1", hostname: "herdr.example.com", service: "herdr-edge-prod" }], onDetach: () => { deleted = true; } }), async () => {
    const good = await detach(c);
    assert.equal(good.ok, true);
    assert.equal(deleted, true);
  });
});

test("detach recovers when DELETE committed but response was lost", async () => {
  const domains = [{ id: "domain1", hostname: "herdr.example.com", service: "herdr-edge-prod" }];
  const c = cfg({ CF_API_BASE: "https://cf.test/client/v4", HERDR_CUSTOM_DOMAIN_STATE_PATH: await statePath() });
  const base = handler({ domains });
  await withFetch(async (input, init = {}) => {
    const u = new URL(typeof input === "string" ? input : input.url);
    if (u.hostname === "cf.test" && u.pathname.endsWith("/workers/domains/domain1") && (init.method || "GET").toUpperCase() === "DELETE") {
      domains.splice(0, domains.length);
      throw new TypeError("response lost after commit");
    }
    return base(input, init);
  }, async () => {
    const r = await detach(c, "test");
    assert.equal(r.ok, true);
    assert.equal(r.recovered, true);
  });
});

test("status is read-only and reports custom domain binding", async () => {
  const c = cfg({ CF_API_BASE: "https://cf.test/client/v4", HERDR_CUSTOM_DOMAIN_STATE_PATH: await statePath() });
  await withFetch(handler({ domains: [{ id: "domain1", hostname: "herdr.example.com", service: "herdr-edge-prod" }] }), async () => {
    const r = await status(c);
    assert.deepEqual({ attached: r.attached, service: r.service, count: r.count }, { attached: true, service: "herdr-edge-prod", count: 1 });
  });
});

test("open-source Worker template defaults to workers.dev and does not require a custom domain", async () => {
  const template = await readFile(new URL("../edge/cloudflare/wrangler.user.example.toml", import.meta.url), "utf8");
  const docs = await readFile(new URL("../docs/i18n/en/cloudflare-edge-deployment.md", import.meta.url), "utf8");
  assert.match(template, /workers_dev = true/);
  assert.match(template, /routes = \[\]/);
  assert.match(template, /YOUR_ACCOUNT_SUBDOMAIN\.workers\.dev/);
  assert.doesNotMatch(template, /agentforme\.cc\.cd/);
  assert.match(docs, /does not require users to own a domain/);
  assert.match(docs, /Custom Domain.*recommended, optional/s);
  assert.match(docs, /workers\.dev.*default, fully supported/s);
});
