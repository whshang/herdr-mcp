// #632 behaviour test: drive the real keepalive entry point (ensureAlive) across
// a runtime boot_id change and count endpoint.register calls at the
// native-host boundary. It slices only regions that exist both before and
// after the fix (recoverOpenChatGptBrowserRegistry .. registration,
// stopPushStream .. ensurePushStream, ensureAlive), so on the pre-fix code it
// fails on the register count, not on extraction.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

const backgroundSource = readFileSync(new URL("../extension/background.js", import.meta.url), "utf8");

function slice(startMarker, endMarker, what) {
  const start = backgroundSource.indexOf(startMarker);
  const end = backgroundSource.indexOf(endMarker, start);
  assert.ok(start >= 0 && end > start, `${what} must remain extractable`);
  return backgroundSource.slice(start, end);
}

function harness() {
  const registrationSource = slice(
    "async function recoverOpenChatGptBrowserRegistry()",
    "\n/** Post one finalized turn",
    "endpoint registration",
  );
  const streamSource = slice("function stopPushStream()", "\nasync function runPushStream(", "push stream lifecycle");
  const aliveSource = slice("async function ensureAlive(", "\n// ---- Install, browser startup", "ensureAlive");
  const calls = { registers: 0, streams: 0 };
  const api = new Function(
    "calls",
    `
    let browserEndpoint = null;
    let pushStream = null;
    let nextRef = 0;
    const configReady = Promise.resolve();
    const STATE_FETCH_MS = 1000;
    const H2W_SCRIPT_VERSION = "test";
    const chrome = { tabs: { query: async () => [] } };
    const conversationInfoForTab = async () => null;
    const browserRegistryUrl = () => "http://local/extension/browser/registry";
    const getOrCreateBrowserProfileSeed = async () => "seed";
    const getBrowserObservationGeneration = async () => 1;
    const callLog = () => {};
    const extensionOwnerAllowsControl = async () => true;
    const fetchStateFresh = async () => ({ ok: true, boot_id: "unused" });
    const loadBindings = async () => ({});
    const reconcileProgressTimers = () => {};
    const runPushStream = async () => { calls.streams += 1; };
    const localHerdrFetch = async (_url, init) => {
      if (JSON.parse(init.body).operation === "endpoint.register") calls.registers += 1;
      nextRef += 1;
      const endpoint = { endpoint_ref: "bep_" + String(nextRef).padStart(64, "0"), consent_revision: 0 };
      return { ok: true, status: 200, json: async () => ({ ok: true, endpoint }) };
    };
    ${registrationSource}
    ${streamSource}
    ${aliveSource}
    return {
      ensureAlive,
      get endpoint() { return browserEndpoint; },
      get pushStream() { return pushStream; },
    };
    `,
  )(calls);
  return { api, calls };
}

async function flush(turns = 30) {
  for (let i = 0; i < turns; i += 1) await Promise.resolve();
}

test("user extension re-registers after a runtime switch without a worker restart | Given a live worker that registered and streamed on runtime boot A | When the keepalive ensureAlive sees runtime boot B | Then exactly one new endpoint.register is sent and a fresh endpoint is cached", async () => {
  const { api, calls } = harness();
  await api.ensureAlive({}, { ok: true, boot_id: "boot-a" });
  assert.equal(calls.registers, 1, "initial registration on boot A");
  api.pushStream.bootId = "boot-a"; // the hello on boot A
  const first = api.endpoint.endpoint_ref;

  await api.ensureAlive({}, { ok: true, boot_id: "boot-b" });
  await flush();
  assert.equal(calls.registers - 1, 1, "expected exactly 1 endpoint.register after the boot_id change");
  assert.notEqual(api.endpoint.endpoint_ref, first);

  await api.ensureAlive({}, { ok: true, boot_id: "boot-b" });
  await flush();
  assert.equal(calls.registers, 2, "same boot_id must not register again");
});
