import assert from "node:assert/strict";
import { mkdtemp, readFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

import { config as domainConfig, CONTRACT_HASH } from "../bin/herdr-cloudflare-domain";
import { executeTransaction } from "../bin/herdr-custom-domain-cutover";

function json(body, status = 200) {
  return new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
}

function tools() { return Array.from({ length: 17 }, (_, i) => ({ name: `tool_${i}` })); }

const rollback = {
  schema_version: 1,
  hostname: "herdr.example.com",
  legacy_record: {
    type: "CNAME",
    name: "herdr",
    target: "tunnel.example.cfargotunnel.com",
    proxied: true,
    ttl: "auto",
  },
};

function cfg(overrides = {}) {
  return domainConfig({
    CLOUDFLARE_API_TOKEN: "domain-token",
    CLOUDFLARE_ACCOUNT_ID: "account1",
    HERDR_CUTOVER_ZONE_ID: "zone1",
    HERDR_CUTOVER_ZONE: "example.com",
    HERDR_CUSTOM_DOMAIN: "herdr.example.com",
    HERDR_CUTOVER_WORKER: "herdr-edge-prod",
    HERDR_CUTOVER_PROD_EDGE: "https://prod.test",
    HERDR_CUTOVER_WORKSTATION: "prod-real-runtime",
    HERDR_CUTOVER_PROBE_BEARER: "probe-secret",
    HERDR_CUTOVER_WATCH_ATTEMPTS: "1",
    HERDR_CUTOVER_FAILURE_THRESHOLD: "1",
    HERDR_CUTOVER_WATCH_INTERVAL_MS: "0",
    ...overrides,
  });
}

async function statePath() {
  const dir = await mkdtemp(join(tmpdir(), "herdr-cutover-txn-"));
  return join(dir, "state.json");
}

function handler({ publicHealthy = true } = {}) {
  let dns = [{
    id: "dns1",
    type: "CNAME",
    name: "herdr.example.com",
    content: "tunnel.example.cfargotunnel.com",
    proxied: true,
    ttl: 1,
  }];
  let domains = [];
  const calls = { dnsDelete: 0, dnsRestore: 0, domainAttach: 0, domainDetach: 0 };

  const fetchImpl = async (input, init = {}) => {
    const u = new URL(typeof input === "string" ? input : input.url);
    const method = (init.method || "GET").toUpperCase();

    if (u.hostname === "api.cloudflare.com") {
      if (u.pathname === "/client/v4/zones/zone1/dns_records" && method === "GET") {
        return json({ success: true, result: dns.filter((r) => !u.searchParams.get("name") || r.name === u.searchParams.get("name")) });
      }
      if (u.pathname === "/client/v4/zones/zone1/dns_records/dns1" && method === "DELETE") {
        calls.dnsDelete += 1;
        dns = [];
        return json({ success: true, result: { id: "dns1" } });
      }
      if (u.pathname === "/client/v4/zones/zone1/dns_records" && method === "POST") {
        calls.dnsRestore += 1;
        const body = JSON.parse(init.body);
        dns = [{ id: "dns2", ...body }];
        return json({ success: true, result: dns[0] });
      }
      if (u.pathname === "/client/v4/accounts/account1/workers/domains" && method === "GET") {
        return json({ success: true, result: domains });
      }
      if (u.pathname === "/client/v4/accounts/account1/workers/domains" && method === "PUT") {
        calls.domainAttach += 1;
        const body = JSON.parse(init.body);
        domains = [{ id: "domain1", hostname: body.hostname, service: body.service }];
        return json({ success: true, result: domains[0] });
      }
      if (u.pathname === "/client/v4/accounts/account1/workers/domains/domain1" && method === "DELETE") {
        calls.domainDetach += 1;
        domains = [];
        return json({ success: true, result: null });
      }
    }

    if (u.hostname === "prod.test" || u.hostname === "herdr.example.com") {
      const healthy = u.hostname === "prod.test" || publicHealthy;
      if (u.pathname === "/health") return healthy ? json({ ok: true, contractEpoch: 1, contractHash: CONTRACT_HASH }) : json({ ok: false }, 503);
      if (u.pathname === "/status/prod-real-runtime") return healthy ? json({ ok: true, online: true, runtimeVersion: "0.3.23", contractEpoch: 1, contractHash: CONTRACT_HASH }) : json({ ok: false }, 503);
      if (u.pathname === "/.well-known/oauth-authorization-server") return json({
        issuer: "https://herdr.example.com",
        authorization_endpoint: "https://herdr.example.com/oauth/authorize",
        token_endpoint: "https://herdr.example.com/oauth/token",
        registration_endpoint: "https://herdr.example.com/oauth/register",
      });
      if (u.pathname === "/.well-known/mcp.json") return json({ serverUrl: "https://herdr.example.com/mcp", name: "herdr-mcp", version: "0.3.23" });
      if (u.pathname === "/mcp" && method === "POST") {
        const body = JSON.parse(init.body);
        if (!healthy) return json({ ok: false }, 503);
        if (body.method === "tools/list") return json({ jsonrpc: "2.0", id: body.id, result: { tools: tools(), _meta: { herdr: { contract_hash: CONTRACT_HASH } } } });
        if (body.method === "tools/call") return json({ jsonrpc: "2.0", id: body.id, result: { content: [{ type: "text", text: "inspect ok 0.3.23" }], isError: false } });
      }
    }

    return json({ error: "unexpected", url: u.href, method }, 500);
  };
  return { fetchImpl, calls, state: () => ({ dns, domains }) };
}

async function withFetch(fetchImpl, fn) {
  const old = globalThis.fetch;
  globalThis.fetch = fetchImpl;
  try { return await fn(); } finally { globalThis.fetch = old; }
}

test("transaction deletes legacy CNAME, attaches Custom Domain, verifies public identity, and keeps rollback tunnel alive", async () => {
  const h = handler({ publicHealthy: true });
  const state = await statePath();
  await withFetch(h.fetchImpl, async () => {
    const result = await executeTransaction(cfg(), "dns-token", rollback, {
      statePath: state,
      tunnelAlive: () => true,
      probeBearer: "probe-secret",
    });
    assert.equal(result.ok, true);
    assert.equal(result.code, "CUTOVER_GREEN");
    assert.equal(result.toolCount, 17);
    assert.equal(h.calls.dnsDelete, 1);
    assert.equal(h.calls.domainAttach, 1);
    assert.equal(h.calls.dnsRestore, 0);
    assert.equal(h.calls.domainDetach, 0);
    assert.equal(h.state().dns.length, 0);
    assert.equal(h.state().domains[0].service, "herdr-edge-prod");
    const saved = JSON.parse(await readFile(state, "utf8"));
    assert.equal(saved.stage, "cutover_green");
  });
});

test("transaction automatically detaches Custom Domain and restores exact CNAME if public watch fails", async () => {
  const h = handler({ publicHealthy: false });
  const state = await statePath();
  await withFetch(h.fetchImpl, async () => {
    const result = await executeTransaction(cfg(), "dns-token", rollback, {
      statePath: state,
      tunnelAlive: () => true,
      probeBearer: "probe-secret",
    });
    assert.equal(result.ok, false);
    assert.equal(result.code, "ROLLBACK_COMPLETE");
    assert.equal(h.calls.dnsDelete, 1);
    assert.equal(h.calls.domainAttach, 1);
    assert.equal(h.calls.domainDetach, 1);
    assert.equal(h.calls.dnsRestore, 1);
    assert.equal(h.state().domains.length, 0);
    assert.equal(h.state().dns.length, 1);
    assert.equal(h.state().dns[0].content, "tunnel.example.cfargotunnel.com");
    assert.equal(h.state().dns[0].proxied, true);
    assert.equal(h.state().dns[0].ttl, 1);
    const saved = JSON.parse(await readFile(state, "utf8"));
    assert.equal(saved.stage, "rollback_complete");
  });
});

test("transaction refuses to mutate when legacy DNS record differs from rollback evidence", async () => {
  const h = handler();
  const badRollback = structuredClone(rollback);
  badRollback.legacy_record.target = "unexpected.example";
  await withFetch(h.fetchImpl, async () => {
    const result = await executeTransaction(cfg(), "dns-token", badRollback, {
      statePath: await statePath(),
      tunnelAlive: () => true,
      probeBearer: "probe-secret",
    });
    assert.equal(result.ok, false);
    assert.equal(result.code, "legacy_dns_mismatch");
    assert.equal(h.calls.dnsDelete, 0);
    assert.equal(h.calls.domainAttach, 0);
  });
});
