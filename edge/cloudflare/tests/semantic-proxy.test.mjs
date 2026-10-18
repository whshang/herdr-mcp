import { test } from "node:test";
import assert from "node:assert/strict";

import {
  SEMANTIC_MAX_REQUEST_BYTES,
  handleSemanticProxy,
  projectSemanticUsage,
  sanitizeSemanticModel,
  semanticBackoffMs,
  semanticModelPinned,
  semanticRouteHealthSnapshot,
} from "../dist/semantic-proxy.js";

// The semantic proxy owns the Worker-wide route pool. These tests drive the real
// handler with a fake device-registry Durable Object (the external boundary) and
// real route parsing / cooldown / budget code, and replace only `fetch` (the
// outbound provider boundary).

const WORKSTATION = "ws-semantic-proxy-test";
const DEVICE_SECRET = "device-secret-value";

function registryNamespace() {
  return {
    idFromName: () => "devices-v1",
    get: () => ({
      async fetch() {
        return new Response(JSON.stringify({
          ok: true,
          device_id: "dev_semanticproxy",
          credential_id: "cred_semanticproxy",
        }), { status: 200, headers: { "content-type": "application/json" } });
      },
    }),
  };
}

function makeEnv(routes) {
  return {
    DEVICE_REGISTRY_DO: registryNamespace(),
    ...(routes === undefined ? {} : { HERDR_SEMANTIC_ROUTES: JSON.stringify(routes) }),
  };
}

function evaluateRequest(body, secret = DEVICE_SECRET) {
  return new Request("https://edge.example/semantic/systemone", {
    method: "POST",
    headers: {
      "content-type": "application/json",
      authorization: `Bearer ${secret}`,
      "x-herdr-workstation": WORKSTATION,
    },
    body: JSON.stringify(body),
  });
}

function statusRequest() {
  return new Request("https://edge.example/semantic/status", {
    method: "GET",
    headers: {
      authorization: `Bearer ${DEVICE_SECRET}`,
      "x-herdr-workstation": WORKSTATION,
    },
  });
}

function route(name, model = "jev-2026-05-01", extra = {}) {
  return {
    name,
    protocol: "decision",
    url: `https://${name}.example/systemone`,
    model,
    api_key: `secret-for-${name}`,
    ...extra,
  };
}

const QUESTIONS = {
  useful: {
    type: "noul",
    instructions: "Is this useful?",
    criteria: { true: "Useful", false: "Not useful" },
  },
};
const BODY = { state: "Choose a useful route.", questions: QUESTIONS };

function providerAnswer(model = "jev-2026-05-01") {
  return new Response(JSON.stringify({
    model,
    answers: { useful: { type: "noul", noul: 0.92 } },
    usage: { prompt_tokens: 12, completion_tokens: 3, total_tokens: 15 },
  }), { status: 200, headers: { "content-type": "application/json" } });
}

async function withFetch(implementation, run) {
  const originalFetch = globalThis.fetch;
  globalThis.fetch = implementation;
  try {
    return await run();
  } finally {
    globalThis.fetch = originalFetch;
  }
}

test("user spreads semantic load | Given three eligible routes | When evaluations run a full cycle | Then each route serves once and the cycle repeats", async () => {
  const env = makeEnv([route("rr-a"), route("rr-b"), route("rr-c")]);
  const calls = [];
  await withFetch(async (input) => {
    calls.push(input instanceof Request ? input.url : String(input));
    return providerAnswer();
  }, async () => {
    for (let index = 0; index < 4; index += 1) {
      const response = await handleSemanticProxy(evaluateRequest(BODY), env);
      assert.equal(response.status, 200);
      assert.equal((await response.json()).answers.useful.noul, 0.92);
    }
  });
  assert.equal(calls.length, 4);
  assert.equal(new Set(calls).size, 3);
  assert.equal(calls[3], calls[0]);
});

test("user fails over past a rate-limited route | Given a 429 route and a healthy route | When evaluation runs twice | Then the second route answers and the throttled route stays in cooldown", async () => {
  const env = makeEnv([route("throttled-a"), route("healthy-b")]);
  const calls = [];
  await withFetch(async (input) => {
    const url = input instanceof Request ? input.url : String(input);
    calls.push(url);
    return url.includes("throttled-a")
      ? new Response(JSON.stringify({ error: "rate limited" }), { status: 429 })
      : providerAnswer();
  }, async () => {
    const first = await handleSemanticProxy(evaluateRequest(BODY), env);
    assert.equal(first.status, 200);
    assert.equal((await first.json()).answers.useful.noul, 0.92);

    const second = await handleSemanticProxy(evaluateRequest(BODY), env);
    assert.equal(second.status, 200);
  });
  const throttledCalls = calls.filter((url) => url.includes("throttled-a")).length;
  assert.equal(throttledCalls, 1);
  assert.equal(calls.filter((url) => url.includes("healthy-b")).length, 2);
});

test("user gets progressive backoff | Given consecutive retryable failures | When each failure records a cooldown | Then the cooldown doubles from 1s and caps at 60s", async () => {
  assert.equal(semanticBackoffMs(1), 1_000);
  assert.equal(semanticBackoffMs(2), 2_000);
  assert.equal(semanticBackoffMs(3), 4_000);
  assert.equal(semanticBackoffMs(4), 8_000);
  assert.equal(semanticBackoffMs(6), 32_000);
  assert.equal(semanticBackoffMs(7), 60_000);
  assert.equal(semanticBackoffMs(50), 60_000);

  // A route that keeps failing is skipped on the next request instead of being
  // retried immediately, and the pool reports that every route is cooling.
  const env = makeEnv([route("always-500")]);
  const calls = [];
  await withFetch(async () => {
    calls.push("attempt");
    return new Response(JSON.stringify({ error: "boom" }), { status: 500 });
  }, async () => {
    const first = await handleSemanticProxy(evaluateRequest(BODY), env);
    assert.equal(first.status, 502);
    const second = await handleSemanticProxy(evaluateRequest(BODY), env);
    assert.equal(second.status, 502);
    assert.equal((await second.json()).code, "semantic_provider_unavailable");
  });
  assert.equal(calls.length, 1);
});

test("user keeps credential failures in a long cooldown | Given a route that returns 500 twice and one that returns 401 | When requests fail over across them | Then the retryable route backs off while the credential route is cooled for five minutes", async () => {
  // 401/403 use the fixed 300s cooldown class; 5xx uses backoff. Both are
  // observable as "attempted once, then skipped".
  for (const [name, status] of [["auth-401", 401], ["boom-500", 500]]) {
    const env = makeEnv([route(name)]);
    let calls = 0;
    await withFetch(async () => {
      calls += 1;
      return new Response(JSON.stringify({ error: "x" }), { status });
    }, async () => {
      await handleSemanticProxy(evaluateRequest(BODY), env);
      await handleSemanticProxy(evaluateRequest(BODY), env);
    });
    assert.equal(calls, 1, `${name} must be attempted once then cooled down`);
  }
});

test("user never exceeds a route rate budget | Given a route with a token budget below one request | When evaluation runs | Then the pool skips it without cooling it down", async () => {
  const env = makeEnv([
    route("tiny-budget", "jev-2026-05-01", { max_tokens_per_second: 1 }),
    route("normal-budget"),
  ]);
  const calls = [];
  await withFetch(async (input) => {
    const url = input instanceof Request ? input.url : String(input);
    calls.push(url);
    return providerAnswer();
  }, async () => {
    for (let index = 0; index < 2; index += 1) {
      const response = await handleSemanticProxy(evaluateRequest(BODY), env);
      assert.equal(response.status, 200);
    }
  });
  assert.equal(calls.filter((url) => url.includes("tiny-budget")).length, 0);
  assert.equal(calls.filter((url) => url.includes("normal-budget")).length, 2);

  // With only the over-budget route the failure is the local budget code, not a
  // provider cooldown: the route is attempted and skipped repeatedly.
  const solo = makeEnv([route("solo-tiny-budget", "jev-2026-05-01", { max_tokens_per_second: 1 })]);
  await withFetch(async () => providerAnswer(), async () => {
    for (let index = 0; index < 2; index += 1) {
      const response = await handleSemanticProxy(evaluateRequest(BODY), solo);
      assert.equal(response.status, 502);
      assert.equal((await response.json()).code, "semantic_route_rate_limited");
    }
  });
});

test("user keeps old semantic secrets working | Given a secret without budget fields and one with budgets | When the routes are parsed | Then both parse and invalid budgets are rejected", async () => {
  const legacy = makeEnv([route("legacy-route")]);
  const legacyStatus = await handleSemanticProxy(statusRequest(), legacy);
  assert.equal(legacyStatus.status, 200);
  const legacyPayload = await legacyStatus.json();
  assert.equal(legacyPayload.ok, true);
  assert.deepEqual(
    legacyPayload.routes.map(({ name, model, pinned }) => ({ name, model, pinned })),
    [{ name: "legacy-route", model: "jev-2026-05-01", pinned: true }],
  );

  const budgeted = makeEnv([
    route("budgeted-route", "jev-2026-05-01", {
      max_requests_per_second: 10,
      max_tokens_per_second: 2_000,
    }),
  ]);
  const budgetedStatus = await handleSemanticProxy(statusRequest(), budgeted);
  assert.equal(budgetedStatus.status, 200);
  assert.equal((await budgetedStatus.json()).routes[0].name, "budgeted-route");

  for (const invalid of [
    { max_requests_per_second: 0 },
    { max_requests_per_second: 1.5 },
    { max_tokens_per_second: "1000" },
    { max_tokens_per_second: -5 },
  ]) {
    const env = makeEnv([route("invalid-budget-route", "jev-2026-05-01", invalid)]);
    const response = await handleSemanticProxy(statusRequest(), env);
    assert.equal(response.status, 503, JSON.stringify(invalid));
    assert.equal((await response.json()).code, "semantic_provider_misconfigured");
  }
});

test("user cannot add unknown route keys | Given a secret with an unsupported key | When status is requested | Then the pool fails closed as misconfigured", async () => {
  const env = makeEnv([{ ...route("unknown-key"), max_rps: 5 }]);
  const response = await handleSemanticProxy(statusRequest(), env);
  assert.equal(response.status, 503);
  assert.equal((await response.json()).code, "semantic_provider_misconfigured");
});

test("user never sees a provider key | Given route credentials used for success, failure, and status | When output is produced | Then no response or log line contains a key", async () => {
  const env = makeEnv([route("log-safe")]);
  const logged = [];
  const originalLog = console.log;
  const originalWarn = console.warn;
  console.log = (...args) => logged.push(args.map(String).join(" "));
  console.warn = (...args) => logged.push(args.map(String).join(" "));
  const responses = [];
  try {
    await withFetch(async () => providerAnswer(), async () => {
      const ok = await handleSemanticProxy(evaluateRequest(BODY), env);
      responses.push(await ok.text());
      const status = await handleSemanticProxy(statusRequest(), env);
      responses.push(await status.text());
    });
    await withFetch(async () => new Response("nope", { status: 500 }), async () => {
      const failed = await handleSemanticProxy(evaluateRequest(BODY), env);
      responses.push(await failed.text());
    });
  } finally {
    console.log = originalLog;
    console.warn = originalWarn;
  }
  const key = "secret-for-log-safe";
  assert.equal(JSON.stringify(responses).includes(key), false);
  assert.equal(logged.join("\n").includes(key), false);
  // The success line still records route, requested/returned model, and usage.
  const success = logged.find((line) => line.includes("semantic_evaluate"));
  assert.ok(success, logged.join("\n"));
  const parsed = JSON.parse(success);
  assert.equal(parsed.route, "log-safe");
  assert.equal(parsed.requested_model, "jev-2026-05-01");
  assert.equal(parsed.returned_model, "jev-2026-05-01");
  assert.equal(parsed.usage.total_tokens, 15);
});

test("user sees model pinning per route | Given floating and versioned models | When status is requested | Then each route reports pinned without rejecting the route", async () => {
  const env = makeEnv([
    route("floating-latest", "jev-latest"),
    route("floating-alias", "JEV-LATEST"),
    route("no-digit", "jev"),
    route("pinned-date", "jev-2026-05-01"),
  ]);
  const response = await handleSemanticProxy(statusRequest(), env);
  assert.equal(response.status, 200);
  const payload = await response.json();
  assert.equal(payload.ok, true);
  assert.deepEqual(
    payload.routes.map(({ name, pinned }) => ({ name, pinned })),
    [
      { name: "floating-latest", pinned: false },
      { name: "floating-alias", pinned: false },
      { name: "no-digit", pinned: false },
      { name: "pinned-date", pinned: true },
    ],
  );
  assert.equal(semanticModelPinned("jev-2026-05-01"), true);
  assert.equal(semanticModelPinned("jev-latest"), false);
  assert.equal(semanticModelPinned("latest"), false);
  assert.equal(semanticModelPinned("jev"), false);
});

test("user is protected from oversized batches | Given a state past the batch guard | When evaluation is requested | Then the proxy rejects it with a stable 413 code", async () => {
  const env = makeEnv([route("state-guard")]);
  let calls = 0;
  await withFetch(async () => {
    calls += 1;
    return providerAnswer();
  }, async () => {
    const tooLarge = await handleSemanticProxy(
      evaluateRequest({ state: "x".repeat(70_000), questions: QUESTIONS }),
      env,
    );
    assert.equal(tooLarge.status, 413);
    assert.equal((await tooLarge.json()).code, "semantic_state_too_large");

    // The per-question guard rejects a state that is small in total but large
    // next to the longest question.
    const longQuestion = {
      useful: {
        type: "noul",
        instructions: "Is this useful?",
        criteria: { true: "y".repeat(33_000), false: "Not useful" },
      },
    };
    const tooLargeForQuestion = await handleSemanticProxy(
      evaluateRequest({ state: "z".repeat(1_000), questions: longQuestion }),
      env,
    );
    assert.equal(tooLargeForQuestion.status, 413);
    assert.equal((await tooLargeForQuestion.json()).code, "semantic_state_too_large");

    const accepted = await handleSemanticProxy(evaluateRequest(BODY), env);
    assert.equal(accepted.status, 200);
  });
  assert.equal(calls, 1);
});

test("user still reaches a healthy route behind over-budget routes | Given three routes whose token budget cannot fit one request and one healthy route | When evaluation runs | Then every call is answered by the healthy route and the skipped routes are never called", async () => {
  const env = makeEnv([
    route("skip-budget-a", "jev-2026-05-01", { max_tokens_per_second: 1 }),
    route("skip-budget-b", "jev-2026-05-01", { max_tokens_per_second: 1 }),
    route("skip-budget-c", "jev-2026-05-01", { max_tokens_per_second: 1 }),
    route("skip-budget-healthy"),
  ]);
  const calls = [];
  await withFetch(async (input) => {
    calls.push(input instanceof Request ? input.url : String(input));
    return providerAnswer();
  }, async () => {
    // The round-robin cursor advances per call, so running once per route covers
    // every start position: a budget skip must not consume one of the three
    // attempt-limit attempts from any of them.
    for (let round = 0; round < 4; round += 1) {
      const response = await handleSemanticProxy(evaluateRequest(BODY), env);
      assert.equal(response.status, 200);
      assert.equal((await response.json()).answers.useful.noul, 0.92);
    }
  });
  assert.equal(calls.length, 4);
  assert.deepEqual(
    new Set(calls),
    new Set(["https://skip-budget-healthy.example/systemone"]),
    "only the healthy route may be called",
  );
});

test("user keeps a long cooldown when failures complete out of order | Given a credential failure and a later retryable failure on one route | When both complete | Then the 300s cooldown survives the shorter backoff", async () => {
  const env = makeEnv([route("out-of-order-health")]);
  const credential = () => new Response(JSON.stringify({ error: "unauthorized" }), { status: 401 });
  const retryable = () => new Response(JSON.stringify({ error: "boom" }), { status: 500 });
  let calls = 0;
  let releaseCredential;
  let releaseRetryable;
  let markBothInFlight;
  const bothInFlight = new Promise((resolve) => {
    markBothInFlight = resolve;
  });
  await withFetch(async () => {
    calls += 1;
    if (calls === 1) {
      return new Promise((resolve) => {
        releaseCredential = () => resolve(credential());
      });
    }
    markBothInFlight();
    return new Promise((resolve) => {
      releaseRetryable = () => resolve(retryable());
    });
  }, async () => {
    // Two requests are in flight on the same route before either failure is
    // recorded, so their completions can arrive in either order.
    const firstRequest = handleSemanticProxy(evaluateRequest(BODY), env);
    const secondRequest = handleSemanticProxy(evaluateRequest(BODY), env);
    await bothInFlight;

    // The credential failure lands first and earns the fixed 300s cooldown.
    releaseCredential();
    assert.equal((await firstRequest).status, 502);

    // The retryable failure finishes afterwards; its 1s backoff must not
    // replace the 300s deadline already in force.
    releaseRetryable();
    assert.equal((await secondRequest).status, 502);

    const health = semanticRouteHealthSnapshot("out-of-order-health");
    assert.equal(health.consecutive_failures, 1);
    assert.ok(
      health.cooldown_remaining_ms > 250_000,
      `cooldown was shortened to ${health.cooldown_remaining_ms}ms`,
    );
  });
  assert.equal(calls, 2);
});

test("user is not rejected for astral characters in state | Given a state of 17,000 emoji | When evaluation is requested | Then the batch guard counts code points and accepts it", async () => {
  const env = makeEnv([route("unicode-guard")]);
  const state = "😀".repeat(17_000);
  // 34,000 UTF-16 code units would trip the 32k guard; 17,000 code points (what
  // the local runtime counts) does not.
  assert.equal(state.length, 34_000);
  let calls = 0;
  await withFetch(async () => {
    calls += 1;
    return providerAnswer();
  }, async () => {
    const response = await handleSemanticProxy(evaluateRequest({ state, questions: QUESTIONS }), env);
    assert.equal(response.status, 200);
    assert.equal((await response.json()).answers.useful.noul, 0.92);
  });
  assert.equal(calls, 1);
});

test("user never sees a credential echoed back through provider usage or model | Given a route key echoed in usage and model | When evaluation runs | Then logs, status, and the response report the configured model and only numeric token counts", async () => {
  const env = makeEnv([route("echo-safe")]);
  const key = "secret-for-echo-safe";
  const logged = [];
  const originalLog = console.log;
  console.log = (...args) => logged.push(args.map(String).join(" "));
  let responseText = "";
  try {
    await withFetch(async () => new Response(JSON.stringify({
      model: key,
      answers: { useful: { type: "noul", noul: 0.92 } },
      usage: {
        prompt_tokens: 5,
        completion_tokens: 2,
        total_tokens: 7,
        diagnostic: `Bearer ${key}`,
        nested: { x: key },
      },
    }), { status: 200, headers: { "content-type": "application/json" } }), async () => {
      const response = await handleSemanticProxy(evaluateRequest(BODY), env);
      assert.equal(response.status, 200);
      responseText = await response.text();
    });
    const status = await handleSemanticProxy(statusRequest(), env);
    const statusText = await status.text();
    assert.equal(statusText.includes(key), false);
  } finally {
    console.log = originalLog;
  }

  // The credential-carrying model falls back to the configured route model and
  // only allowlisted numeric token counts survive the usage projection.
  const payload = JSON.parse(responseText);
  assert.equal(payload.model, "jev-2026-05-01");
  assert.deepEqual(payload.usage, { prompt_tokens: 5, completion_tokens: 2, total_tokens: 7 });
  assert.equal(responseText.includes(key), false);

  const success = logged.find((line) => line.includes("semantic_evaluate"));
  assert.ok(success, logged.join("\n"));
  const parsed = JSON.parse(success);
  assert.equal(parsed.route, "echo-safe");
  assert.equal(parsed.returned_model, "jev-2026-05-01");
  assert.deepEqual(parsed.usage, { prompt_tokens: 5, completion_tokens: 2, total_tokens: 7 });
  assert.equal(logged.join("\n").includes(key), false);

  // The projection and sanitizer are the shared gate both paths use.
  assert.deepEqual(
    projectSemanticUsage({ prompt_tokens: 5, diagnostic: key, nested: { x: key } }),
    { prompt_tokens: 5 },
  );
  assert.equal(projectSemanticUsage({ diagnostic: key }), null);
  assert.equal(projectSemanticUsage({ prompt_tokens: -1, total_tokens: 2.5 }), null);
  assert.equal(sanitizeSemanticModel(key, key), null);
  assert.equal(sanitizeSemanticModel("!!!", key), null);
  assert.equal(sanitizeSemanticModel("jev-2026-05-01", key), "jev-2026-05-01");
  assert.equal(sanitizeSemanticModel(`jev ${"x".repeat(200)}`, key).length, 128);
});

test("user always receives a non-empty model name | Given a provider model that sanitizes away or echoes the route key | When evaluation runs | Then the response and log report the configured model and never the key", async () => {
  const env = makeEnv([route("fallback-model", "jev-2026-05-01")]);
  const key = "secret-for-fallback-model";
  const logged = [];
  const originalLog = console.log;
  console.log = (...args) => logged.push(args.map(String).join(" "));
  try {
    for (const providerModel of ["!!!", key]) {
      await withFetch(async () => new Response(JSON.stringify({
        model: providerModel,
        answers: { useful: { type: "noul", noul: 0.92 } },
      }), { status: 200, headers: { "content-type": "application/json" } }), async () => {
        const response = await handleSemanticProxy(evaluateRequest(BODY), env);
        assert.equal(response.status, 200);
        const text = await response.text();
        const payload = JSON.parse(text);
        // Never null (released runtimes require a string) and never the key.
        assert.equal(payload.model, "jev-2026-05-01");
        assert.equal(text.includes(key), false);
      });
    }
  } finally {
    console.log = originalLog;
  }

  const models = logged
    .filter((line) => line.includes("semantic_evaluate"))
    .map((line) => JSON.parse(line).returned_model);
  assert.equal(models.length, 2, logged.join("\n"));
  assert.deepEqual(models, ["jev-2026-05-01", "jev-2026-05-01"]);
  assert.equal(logged.join("\n").includes(key), false);
});

test("user is accepted at the runtime request byte budget | Given an evaluate body at the 88 KiB runtime budget | When the proxy receives it | Then it is answered instead of rejected with 413", async () => {
  const env = makeEnv([route("byte-budget")]);
  // One emoji is four UTF-8 bytes but one code point: the state fills the byte
  // budget without touching the character guard.
  const state = "😀".repeat(Math.floor((SEMANTIC_MAX_REQUEST_BYTES - 512) / 4));
  const body = { state, questions: QUESTIONS };
  const bodyBytes = Buffer.byteLength(JSON.stringify(body));
  assert.ok(bodyBytes <= SEMANTIC_MAX_REQUEST_BYTES, `${bodyBytes} bytes`);
  assert.ok(bodyBytes > SEMANTIC_MAX_REQUEST_BYTES - 1024, `${bodyBytes} bytes`);

  let calls = 0;
  await withFetch(async () => {
    calls += 1;
    return providerAnswer();
  }, async () => {
    const response = await handleSemanticProxy(evaluateRequest(body), env);
    assert.notEqual(response.status, 413);
    assert.equal(response.status, 200);
    assert.equal((await response.json()).answers.useful.noul, 0.92);
  });
  assert.equal(calls, 1);
});
