import { readFileSync } from "node:fs";
import { test } from "node:test";
import assert from "node:assert/strict";

import { computeContractHash } from "../dist/relay/contract.js";
import { ExponentialBackoff } from "../dist/link/backoff.js";
import { RuntimeGenerationManager } from "../dist/link/runtime-generation.js";

const fixture = JSON.parse(
  readFileSync(new URL("./fixtures/link-reliability-batch4.json", import.meta.url), "utf8"),
);

const TOKEN = "link-reliability-batch4-token";
const CATALOG = [{
  name: "herdr_inspect",
  description: "inspect",
  inputSchema: { type: "object", properties: {} },
}];
const HASH = computeContractHash(CATALOG);

function frame(id, operation = "herdr_inspect") {
  return { v: 1, type: "request", workstation_id: "w1", request_id: id, operation, arguments: {} };
}

function json(body, status = 200) {
  return new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
}

function runtimeFetch(options = {}) {
  const state = {
    healthCalls: new Map(),
    toolCalls: [],
    pendingOld: null,
    releaseOld: null,
    ...options.state,
  };
  const fn = async (input, init = {}) => {
    const u = new URL(typeof input === "string" ? input : input.url);
    const port = u.port;
    const body = JSON.parse(init.body || "{}");
    if (body.method === "server/discover") {
      const n = (state.healthCalls.get(port) || 0) + 1;
      state.healthCalls.set(port, n);
      const version = port === "8773" ? "0.3.26" : "0.3.23";
      return json({ jsonrpc: "2.0", id: body.id, result: { serverInfo: { name: "herdr", version } } });
    }
    if (body.method === "tools/list") {
      return json({ jsonrpc: "2.0", id: body.id, result: { tools: CATALOG } });
    }
    if (body.method === "tools/call") {
      state.toolCalls.push({ port, name: body.params?.name, id: body.id });
      if (options.deferPort === port) {
        return new Promise((resolve) => {
          state.pendingOld = { resolve, id: body.id, port };
          state.releaseOld = () => resolve(json({ jsonrpc: "2.0", id: body.id, result: { port } }));
          init.signal?.addEventListener("abort", () => resolve(json({ jsonrpc: "2.0", id: body.id, error: { code: -32800 } })), { once: true });
        });
      }
      return json({ jsonrpc: "2.0", id: body.id, result: { port } });
    }
    return json({ error: "unexpected" }, 500);
  };
  fn.state = state;
  return fn;
}

function manager(fetchFn, base) {
  return new RuntimeGenerationManager({
    base,
    bearerToken: TOKEN,
    contractHash: HASH,
    fetch: fetchFn,
    observationChecks: 2,
    observationIntervalMs: 0,
    sleep: async () => {},
  });
}

function rngFrom(value) {
  return () => value;
}

function buildBackoff(options) {
  const opts = { ...options };
  if (opts.rng !== undefined) opts.rng = rngFrom(opts.rng);
  return new ExponentialBackoff(opts);
}

function assertSanitized(backoff, expected) {
  assert.equal(backoff.baseMs, expected.baseMs, "baseMs");
  assert.equal(backoff.maxMs, expected.maxMs, "maxMs");
  assert.equal(backoff.factor, expected.factor, "factor");
  assert.equal(backoff.jitter, expected.jitter, "jitter");
}

test("backoff sanitization cases match Node oracle", () => {
  for (const entry of fixture.backoff.sanitization_cases) {
    assert.equal(entry.oracle, "node_parity", `${entry.name}: oracle must be node_parity`);
    const b = buildBackoff(entry.options);
    assertSanitized(b, entry.expected.sanitized);
    for (const p of entry.expected.peek) {
      assert.equal(b.peek(p.attempt), p.delay, `${entry.name}: peek(${p.attempt})`);
    }
  }
});

test("backoff peek cases match Node oracle", () => {
  for (const entry of fixture.backoff.peek_cases) {
    assert.equal(entry.oracle, "node_parity", `${entry.name}: oracle must be node_parity`);
    const b = buildBackoff(entry.options);
    for (const p of entry.peek) {
      assert.equal(b.peek(p.attempt), p.delay, `${entry.name}: peek(${p.attempt})`);
    }
  }
});

test("backoff sequence cases match Node oracle", () => {
  for (const entry of fixture.backoff.sequence_cases) {
    assert.equal(entry.oracle, "node_parity", `${entry.name}: oracle must be node_parity`);
    const b = buildBackoff(entry.options);
    for (const step of entry.steps) {
      switch (step.op) {
        case "assert_attempt":
          assert.equal(b.attempt, step.value, `${entry.name}: attempt`);
          break;
        case "next":
          assert.equal(b.next(), step.expected, `${entry.name}: next()`);
          break;
        case "reset":
          b.reset();
          break;
        default:
          assert.fail(`${entry.name}: unknown step op ${step.op}`);
      }
    }
  }
});

test("runtime-generation node_parity scenarios match Node oracle", async () => {
  const scenarios = fixture.runtime_generation.scenarios.filter((s) => s.oracle === "node_parity");
  assert.ok(scenarios.length >= 4, "expected at least 4 node_parity scenarios");

  for (const scenario of scenarios) {
    const fetchFn = runtimeFetch({ deferPort: "8772" });
    const m = manager(fetchFn, scenario.setup.base);
    const pendingDispatches = new Map();

    for (const step of scenario.steps) {
      switch (step.op) {
        case "register": {
          const spec = scenario.setup.candidates.find((c) => c.generation === step.generation);
          assert.ok(spec, `${scenario.name}: candidate spec for ${step.generation}`);
          const res = await m.registerGeneration(spec);
          assert.equal(res.ok, true, `${scenario.name}: register ${step.generation}`);
          break;
        }
        case "dispatch": {
          const p = m.dispatchRequest(frame(step.request_id));
          if (step.defer) {
            pendingDispatches.set(step.request_id, p);
            await new Promise((resolve) => setImmediate(resolve));
            assert.ok(fetchFn.state.pendingOld, `${scenario.name}: deferred request pending`);
          } else {
            const res = await p;
            if (step.expect_port !== undefined) {
              assert.deepEqual(res, { ok: true, result: { port: step.expect_port } }, `${scenario.name}: dispatch ${step.request_id}`);
            }
          }
          break;
        }
        case "activate": {
          const res = await m.activateGeneration(step.generation, { checks: step.checks, intervalMs: 0 });
          assert.equal(res.ok, true, `${scenario.name}: activate ${step.generation}`);
          break;
        }
        case "release": {
          assert.ok(fetchFn.state.releaseOld, `${scenario.name}: releaseOld present`);
          fetchFn.state.releaseOld();
          const p = pendingDispatches.get(step.request_id);
          assert.ok(p, `${scenario.name}: pending dispatch for ${step.request_id}`);
          const res = await p;
          assert.deepEqual(res, { ok: true, result: { port: step.expect_port } }, `${scenario.name}: release ${step.request_id}`);
          break;
        }
        case "cancel": {
          await m.cancelRequest(step.request_id, "test cancel");
          const p = pendingDispatches.get(step.request_id);
          if (p) {
            const res = await p;
            if (step.expect_code !== undefined) {
              assert.equal(res.ok, false, `${scenario.name}: cancel ${step.request_id} fails closed`);
              assert.equal(res.code, step.expect_code, `${scenario.name}: cancel ${step.request_id} code`);
            }
          }
          break;
        }
        case "assert_status": {
          const status = m.getStatus();
          if (step.active_generation !== undefined) assert.equal(status.active_generation, step.active_generation, `${scenario.name}: active_generation`);
          if (step.previous_generation !== undefined) assert.equal(status.previous_generation, step.previous_generation, `${scenario.name}: previous_generation`);
          if (step.last_good_generation !== undefined) assert.equal(status.last_good_generation, step.last_good_generation, `${scenario.name}: last_good_generation`);
          if (step.transition_seq !== undefined) assert.equal(status.transition_seq, step.transition_seq, `${scenario.name}: transition_seq`);
          if (step.generation_phase) {
            for (const [gen, phase] of Object.entries(step.generation_phase)) {
              const rec = status.generations.find((g) => g.generation === gen);
              assert.ok(rec, `${scenario.name}: generation ${gen} present`);
              assert.equal(rec.phase, phase, `${scenario.name}: phase of ${gen}`);
            }
          }
          if (step.generation_in_flight) {
            for (const [gen, n] of Object.entries(step.generation_in_flight)) {
              const rec = status.generations.find((g) => g.generation === gen);
              assert.ok(rec, `${scenario.name}: generation ${gen} present`);
              assert.equal(rec.in_flight, n, `${scenario.name}: in_flight of ${gen}`);
            }
          }
          break;
        }
        default:
          assert.fail(`${scenario.name}: unknown step op ${step.op}`);
      }
    }
  }
});

test("runtime-generation rust_safety_strengthening scenarios assert schema/intent only", () => {
  const scenarios = fixture.runtime_generation.scenarios.filter((s) => s.oracle === "rust_safety_strengthening");
  assert.ok(scenarios.length >= 2, "expected at least 2 rust_safety_strengthening scenarios");

  for (const scenario of scenarios) {
    assert.equal(scenario.oracle, "rust_safety_strengthening", `${scenario.name}: oracle`);
    assert.ok(typeof scenario.intent === "string" && scenario.intent.length > 0, `${scenario.name}: intent present`);
    assert.ok(typeof scenario.desired_invariant === "string" && scenario.desired_invariant.length > 0, `${scenario.name}: desired_invariant present`);
    assert.ok(!scenario.steps, `${scenario.name}: safety-strengthening scenarios must not carry executable steps`);
  }
});
