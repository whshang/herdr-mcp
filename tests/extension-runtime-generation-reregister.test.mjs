// #632: the extension must re-register its browser endpoint when the runtime
// generation changes (boot_id) or the runtime says the endpoint is not
// registered, without a service-worker restart. These tests slice the real
// background.js code and drive it with contract-checked fakes at the
// native-host boundary (no mocking APIs).
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

const backgroundSource = readFileSync(new URL("../extension/background.js", import.meta.url), "utf8");

function sliceSource(startMarker, endMarker, what) {
  const start = backgroundSource.indexOf(startMarker);
  const end = backgroundSource.indexOf(endMarker, start);
  assert.ok(start >= 0 && end > start, `${what} must remain extractable`);
  return backgroundSource.slice(start, end);
}

function harness() {
  const reconciliationSource = sliceSource(
    "// ---- #632 browser endpoint runtime reconciliation (begin) ----",
    "// ---- #632 browser endpoint runtime reconciliation (end) ----",
    "browser endpoint runtime reconciliation",
  );
  const registerOnceSource = sliceSource(
    "async function registerLocalBrowserEndpointOnce()",
    "\n/** Post one finalized turn",
    "single registration attempt",
  );
  const runtimeSource = sliceSource(
    "function reconcilePushStreamRuntime(state)",
    "\nasync function ensurePushStream(",
    "push stream runtime reconciliation",
  );
  const calls = { registers: [], recoveries: 0, stops: 0, ensures: 0, logs: [], failNextRegister: 0 };
  const gates = [];
  const api = new Function(
    "calls",
    "gates",
    `
    let browserEndpoint = null;
    let pushStream = null;
    let nextRef = 0;
    const configReady = Promise.resolve();
    const STATE_FETCH_MS = 1000;
    const H2W_SCRIPT_VERSION = "test";
    const browserRegistryUrl = () => "http://local/extension/browser/registry";
    const getOrCreateBrowserProfileSeed = async () => "seed";
    const getBrowserObservationGeneration = async () => 1;
    const recoverOpenChatGptBrowserRegistry = async () => { calls.recoveries += 1; return { observed: 1 }; };
    const callLog = (...args) => calls.logs.push(args.join(" "));
    const stopPushStream = () => { calls.stops += 1; pushStream = null; };
    const loadBindings = async () => ({});
    const ensurePushStream = async () => { calls.ensures += 1; };
    const localHerdrFetch = async (_url, init) => {
      const body = JSON.parse(init.body);
      calls.registers.push(body.operation);
      if (calls.failNextRegister) {
        calls.failNextRegister -= 1;
        throw new Error("native-fetch-failed");
      }
      if (gates.length) await gates.shift();
      nextRef += 1;
      const endpoint = { endpoint_ref: "bep_" + String(nextRef).padStart(64, "0"), consent_revision: 0 };
      return { ok: true, status: 200, json: async () => ({ ok: true, endpoint }) };
    };
    ${registerOnceSource}
    ${reconciliationSource}
    ${runtimeSource}
    return {
      hello: reconcileBrowserEndpointForHello,
      reconcileRuntime: reconcilePushStreamRuntime,
      register: registerLocalBrowserEndpoint,
      current: currentBrowserEndpoint,
      get endpoint() { return browserEndpoint; },
      setPushStream(value) { pushStream = value; },
      get pushStream() { return pushStream; },
    };
    `,
  )(calls, gates);
  return { api, calls, gates };
}

async function flush(turns = 20) {
  for (let i = 0; i < turns; i += 1) await Promise.resolve();
}

test("user re-registers once after a runtime generation change | Given a cached endpoint registered on runtime boot A | When the push hello reports boot B | Then the cache is cleared and endpoint.register runs exactly once", async () => {
  const { api, calls } = harness();
  const ctrl = {};
  api.setPushStream({ ctrl, bootId: null });
  await api.register();
  await api.hello({ boot_id: "boot-a", endpoint_registered: true }, ctrl);
  const first = api.endpoint.endpoint_ref;
  assert.equal(calls.registers.length, 1);
  const pending = api.hello({ boot_id: "boot-b", endpoint_registered: true }, ctrl);
  assert.equal(api.endpoint, null, "cached endpoint cleared on boot change");
  await pending;
  await flush();
  assert.equal(calls.registers.length, 2);
  assert.notEqual(api.endpoint.endpoint_ref, first);
  // The same boot_id seen again (e.g. the reconnect hello) never re-registers.
  await api.hello({ boot_id: "boot-b", endpoint_registered: true }, ctrl);
  await api.hello({ boot_id: "boot-b", endpoint_registered: false }, ctrl);
  await flush();
  assert.equal(calls.registers.length, 2, "one re-register per boot_id");
});

test("user re-registers when the runtime reports the endpoint unregistered | Given a cached endpoint and an unchanged boot_id | When the hello carries endpoint_registered false | Then the extension re-registers and reconnects the push stream", async () => {
  const { api, calls } = harness();
  const ctrl = {};
  api.setPushStream({ ctrl, bootId: null });
  await api.register();
  await api.hello({ boot_id: "boot-a", endpoint_registered: true }, ctrl);
  assert.equal(calls.registers.length, 1);
  await api.hello({ boot_id: "boot-a", endpoint_registered: false }, ctrl);
  await flush();
  assert.equal(calls.registers.length, 2);
  assert.ok(calls.recoveries >= 2, "open ChatGPT tabs are re-observed after re-register");
  assert.equal(calls.stops, 1, "stream reconnects with the fresh endpoint_ref");
  assert.equal(calls.ensures, 1);
});

test("user recovers browser delivery when the first re-registration fails | Given a shared push stream the runtime reported as endpoint_registered false | When the first re-registration attempt fails and a later keepalive registration succeeds | Then that stream is replaced so the runtime evaluates the registered endpoint again", async () => {
  const { api, calls } = harness();
  const ctrl = {};
  api.setPushStream({ ctrl, bootId: "boot-a" });
  await api.register();
  assert.equal(calls.registers.length, 1);
  // Runtime generation switch: the hello for boot-b reports the endpoint as
  // unregistered, and that first re-registration attempt fails at the native
  // boundary (runtime restarting, native fetch refused).
  calls.failNextRegister = 1;
  await api.hello({ boot_id: "boot-b", endpoint_registered: false }, ctrl);
  await flush();
  assert.equal(calls.registers.length, 2, "the first re-registration attempt is sent");
  assert.equal(api.endpoint, null, "a failed registration must not keep a cached endpoint");
  assert.equal(calls.stops, 0, "no stream refresh is claimed for a failed registration");
  // The keepalive path re-registers later and succeeds; the stream the runtime
  // opened as unregistered can still never receive actuation, so it must be
  // replaced once a registration actually succeeded.
  const endpoint = await api.current();
  await flush();
  assert.equal(calls.registers.length, 3);
  assert.ok(endpoint?.endpoint_ref, "the later registration returns an endpoint");
  assert.equal(calls.stops, 1, "the stream opened as unregistered is replaced");
  assert.equal(calls.ensures, 1);
});

test("user gets a single registration for concurrent triggers | Given a slow endpoint registration in flight | When the hello, the runtime boot check and the stream reconnect all request registration | Then only one endpoint.register is sent and every caller gets the same endpoint", async () => {
  const { api, calls, gates } = harness();
  let release;
  gates.push(new Promise((resolve) => { release = resolve; }));
  const ctrl = {};
  api.setPushStream({ ctrl, bootId: "boot-a" });
  const a = api.register();
  const b = api.hello({ boot_id: "boot-b", endpoint_registered: false }, ctrl);
  api.reconcileRuntime({ boot_id: "boot-b" });
  const c = api.current();
  const d = api.register();
  await flush();
  assert.equal(calls.registers.length, 1, "single-flight");
  release();
  const [ra, , rc, rd] = await Promise.all([a, b, c, d]);
  await flush();
  assert.equal(calls.registers.length, 1);
  assert.equal(ra.endpoint_ref, rc.endpoint_ref);
  assert.equal(ra.endpoint_ref, rd.endpoint_ref);
});

test("user keeps the cached endpoint when nothing changed | Given a registered endpoint on boot A | When later hellos report boot A and endpoint_registered true | Then no extra endpoint.register is sent", async () => {
  const { api, calls } = harness();
  const ctrl = {};
  api.setPushStream({ ctrl, bootId: null });
  await api.register();
  for (let i = 0; i < 3; i += 1) await api.hello({ boot_id: "boot-a", endpoint_registered: true }, ctrl);
  assert.equal(api.reconcileRuntime({ boot_id: "boot-a" }), false);
  await api.current();
  await flush();
  assert.equal(calls.registers.length, 1);
  assert.equal(calls.stops, 0);
});

test("user re-registers after a runtime switch detected by the keepalive | Given a cached endpoint and a push stream on boot A | When ensureAlive's state check reports boot B | Then the cache is cleared and the next endpoint lookup registers once", async () => {
  const { api, calls } = harness();
  api.setPushStream({ ctrl: {}, bootId: "boot-a" });
  await api.register();
  assert.equal(api.reconcileRuntime({ boot_id: "boot-b" }), true);
  assert.equal(api.endpoint, null);
  await api.current();
  await flush();
  assert.equal(calls.registers.length, 2);
});
