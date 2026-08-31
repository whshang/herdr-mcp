import { test } from "node:test";
import assert from "node:assert/strict";

import { DeviceRegistryDO } from "../dist/device-registry-do.js";
import { newPairingCode, isPairingCode } from "../dist/device-crypto.js";
import { sanitize } from "../dist/logger.js";
import { sha256Hex } from "../dist/device-crypto.js";

class FakeStorage {
  constructor() {
    this.map = new Map();
    this.writeCount = 0;
    this._queue = Promise.resolve();
  }
  async get(key) { return this.map.get(key); }
  async put(key, value) { this.writeCount += 1; this.map.set(key, structuredClone(value)); }
  async delete(key) { this.writeCount += 1; return this.map.delete(key); }
  async list({ prefix } = {}) {
    return new Map([...this.map].filter(([key]) => !prefix || key.startsWith(prefix)));
  }
  transaction(fn) {
    const run = this._queue.then(() => fn(this));
    this._queue = run.then(() => undefined, () => undefined);
    return run;
  }
  async getAlarm() { return null; }
  async setAlarm() { throw new Error("pairing flow must not schedule alarms"); }
  async deleteAlarm() {}
}

function makeRegistry() {
  const storage = new FakeStorage();
  return { storage, registry: new DeviceRegistryDO({ storage }, {}) };
}

const CONTEXT = "herdr-edge@test";

async function createPairing(registry, overrides = {}) {
  const response = await registry.fetch(new Request("https://registry.internal/internal/devices/pairings", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ worker_context: CONTEXT, ...overrides }),
  }));
  return { response, body: await response.json() };
}

async function consumePairing(registry, overrides = {}) {
  const response = await registry.fetch(new Request("https://registry.internal/internal/devices/pairings/consume", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ worker_context: CONTEXT, ...overrides }),
  }));
  return { response, body: await response.json() };
}

function pairingEntry(storage) {
  const entry = [...storage.map.entries()].find(([key]) => key.startsWith("pairing:"));
  return entry ?? null;
}

test("pairing code is exactly six CSPRNG decimal digits with leading zeros and no modulo bias", () => {
  const codes = new Set();
  for (let i = 0; i < 600; i += 1) codes.add(newPairingCode());
  for (const code of codes) assert.match(code, /^[0-9]{6}$/);
  assert.ok([...codes].some((code) => code.startsWith("0")), "leading zeros must occur");
  assert.ok([...codes].some((code) => code.endsWith("0")));
  // Uniformity sanity: ~half of codes should be < 500000 (no low/odd bias).
  const low = [...codes].filter((code) => Number(code) < 500000).length;
  assert.ok(low > 200 && low < 500, `low-half share out of range: ${low}/${codes.size}`);
  assert.ok(isPairingCode("000000") && isPairingCode("999999"));
  assert.equal(isPairingCode("12345"), false);
  assert.equal(isPairingCode("1234567"), false);
  assert.equal(isPairingCode("12a456"), false);
});

test("pairing id carries at least 128 bits of entropy", async () => {
  const { registry } = makeRegistry();
  const { body } = await createPairing(registry, { ttl_seconds: 60 });
  assert.match(body.pairing_id, /^pair_[0-9a-f]{64}$/, "pair_<256-bit hex> id");
});

test("pairing TTL defaults to 600s and accepts only 60..600 seconds", async () => {
  const { registry } = makeRegistry();
  const { body: defaulted } = await createPairing(registry);
  const ttlSec = (defaulted.expires_at_ms - Date.now()) / 1000;
  assert.ok(ttlSec > 598 && ttlSec <= 600.5, `default TTL must be 600s, got ${ttlSec}`);

  for (const ttl of [60, 300, 600]) {
    const { response } = await createPairing(registry, { ttl_seconds: ttl });
    assert.equal(response.status, 200, `ttl_seconds=${ttl} accepted`);
  }
  for (const ttl of [59, 601, 0, -60, 10.5, "600", null]) {
    const { response, body } = await createPairing(registry, { ttl_seconds: ttl });
    assert.equal(response.status, 400, `ttl_seconds=${String(ttl)} rejected`);
    assert.equal(body.code, "invalid_pairing_ttl");
  }
});

test("storage keeps only digest-keyed verifier records; snapshot cannot be brute-forced from code alone", async () => {
  const { storage, registry } = makeRegistry();
  const { body: session } = await createPairing(registry, { ttl_seconds: 600, name: "audit" });

  const entry = pairingEntry(storage);
  assert.ok(entry, "a pairing record exists");
  const [key, record] = entry;
  assert.match(key, /^pairing:[0-9a-f]{64}$/, "key is sha256(raw pairing_id), never the raw id");

  const snapshot = JSON.stringify([...storage.map]);
  assert.equal(snapshot.includes(session.pairing_id), false);
  assert.equal(snapshot.includes(session.code), false);

  // The verifier must not be derivable from the six-digit code (or code+context)
  // alone: it is HMAC-SHA256 keyed by the raw pairing_id, which is absent from
  // storage, so a snapshot cannot enumerate the code offline.
  const hmac = async (keyText, message) => {
    const key = await crypto.subtle.importKey("raw", new TextEncoder().encode(keyText), { name: "HMAC", hash: "SHA-256" }, false, ["sign"]);
    const mac = await crypto.subtle.sign("HMAC", key, new TextEncoder().encode(message));
    return [...new Uint8Array(mac)].map((b) => b.toString(16).padStart(2, "0")).join("");
  };
  const candidates = [
    await sha256Hex(session.code),
    await sha256Hex(`${CONTEXT}:${session.code}`),
    await sha256Hex(session.code + CONTEXT),
    await hmac(session.code, `${CONTEXT}:${session.code}`),
    await hmac(`${CONTEXT}:${session.code}`, session.code),
  ];
  for (const candidate of candidates) {
    assert.notEqual(candidate, record.verifier_sha256);
  }
  // Internal consistency: HMAC(key = raw pairing_id, message = context || ":" || code).
  const expected = await hmac(session.pairing_id, `${CONTEXT}:${session.code}`);
  assert.equal(expected, record.verifier_sha256);
});

test("wrong codes increment a per-session counter; the fifth locks permanently and retries never reset", async () => {
  const { storage, registry } = makeRegistry();
  const { body: session } = await createPairing(registry, { ttl_seconds: 600 });

  // Wrong codes 1..4 keep the session pending (counter never resets).
  for (let attempt = 1; attempt <= 4; attempt += 1) {
    const { response, body } = await consumePairing(registry, {
      pairing_id: session.pairing_id,
      code: String((Number(session.code) + attempt) % 1000000).padStart(6, "0"),
    });
    assert.equal(response.status, 401);
    assert.equal(body.code, "pairing_rejected");
    assert.equal(pairingEntry(storage)[1].attempts, attempt, `attempt counter is ${attempt}`);
    assert.equal(pairingEntry(storage)[1].state, "pending");
  }

  // 5th wrong code permanently locks; the correct code afterwards still fails.
  const fifth = await consumePairing(registry, { pairing_id: session.pairing_id, code: "000001" });
  assert.equal(fifth.response.status, 401);
  assert.equal(fifth.body.code, "pairing_rejected");
  const locked = pairingEntry(storage)[1];
  assert.equal(locked.state, "locked");
  assert.equal(locked.attempts, 5);

  const afterLock = await consumePairing(registry, { pairing_id: session.pairing_id, code: session.code });
  assert.equal(afterLock.response.status, 401);
  assert.equal(afterLock.body.code, "pairing_rejected");
  const stillLocked = pairingEntry(storage)[1];
  assert.equal(stillLocked.state, "locked");
  assert.equal(stillLocked.attempts, 5, "locked state is terminal and the counter never resets");
});

test("one to four wrong attempts do not prevent consumption with the correct code", async () => {
  const { registry } = makeRegistry();
  const { body: session } = await createPairing(registry, { ttl_seconds: 600 });
  for (const wrong of ["000002", "000003"]) {
    const { response } = await consumePairing(registry, { pairing_id: session.pairing_id, code: wrong });
    assert.equal(response.status, 401);
  }
  const ok = await consumePairing(registry, { pairing_id: session.pairing_id, code: session.code });
  assert.equal(ok.response.status, 200);
  assert.equal(ok.body.workstation_id, ok.body.device_id);
});

test("expired pairing fails closed, is deleted, and never leaks a distinct error code", async () => {
  const { storage, registry } = makeRegistry();
  const { body: session } = await createPairing(registry, { ttl_seconds: 600 });
  const entry = pairingEntry(storage);
  const expiredRecord = { ...entry[1], expires_at_ms: Date.now() - 1 };
  storage.map.set(entry[0], expiredRecord);

  const { response, body } = await consumePairing(registry, { pairing_id: session.pairing_id, code: session.code });
  assert.equal(response.status, 401, "expired pairings use the same generic rejection as everything else");
  assert.equal(body.code, "pairing_rejected");
  assert.equal(storage.map.has(entry[0]), false, "expired record is deleted on consumption attempt");
});

test("expired pairings are rejected even with the correct code and context", async () => {
  const { storage, registry } = makeRegistry();
  const { body: session } = await createPairing(registry, { ttl_seconds: 60 });
  const entry = pairingEntry(storage);
  storage.map.set(entry[0], { ...entry[1], expires_at_ms: 1 });
  const { response } = await consumePairing(registry, { pairing_id: session.pairing_id, code: session.code });
  assert.equal(response.status, 401);
});

test("concurrent double consume yields exactly one success (atomic single-use)", async () => {
  const { storage, registry } = makeRegistry();
  const { body: session } = await createPairing(registry, { ttl_seconds: 600 });

  const attempts = Array.from({ length: 5 }, () =>
    consumePairing(registry, { pairing_id: session.pairing_id, code: session.code }));
  const results = await Promise.all(attempts);
  const successes = results.filter((r) => r.response.status === 200);
  assert.equal(successes.length, 1, "exactly one concurrent consume succeeds");
  for (const r of results) {
    if (r.response.status !== 200) assert.equal(r.body.code, "pairing_rejected");
  }
  assert.equal(pairingEntry(storage), null, "pairing record is fully consumed");
  const deviceKeys = [...storage.map.keys()].filter((key) => key.startsWith("device:"));
  assert.equal(deviceKeys.length, 1, "exactly one device was minted");
});

test("wrong pairing_id, mismatched session, and cross-Worker context all fail closed without enumeration", async () => {
  const { storage, registry } = makeRegistry();
  const unknown = await consumePairing(registry, { pairing_id: `pair_${"a".repeat(64)}`, code: "123456" });
  assert.equal(unknown.response.status, 401);
  assert.equal(unknown.body.code, "pairing_rejected");

  const malformed = await consumePairing(registry, { pairing_id: "not-a-pairing-id", code: "123456" });
  assert.equal(malformed.response.status, 400);
  assert.equal(malformed.body.code, "invalid_pairing_request");

  const { body: a } = await createPairing(registry, { ttl_seconds: 600 });
  const { body: b } = await createPairing(registry, { ttl_seconds: 600 });

  // A stale id mixed with B's code is indistinguishable from any rejection and
  // leaves B's record untouched (zero attempts added).
  const mixed = await consumePairing(registry, { pairing_id: a.pairing_id, code: b.code });
  assert.equal(mixed.response.status, 401);
  assert.equal(mixed.body.code, "pairing_rejected");
  const bRecord = pairingEntry(storage)[1];
  assert.equal(bRecord.attempts, 0, "probes with a foreign pairing_id must not touch the live session");

  const cross = await registry.fetch(new Request("https://registry.internal/internal/devices/pairings/consume", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ pairing_id: b.pairing_id, code: b.code, worker_context: "other-worker@prod" }),
  }));
  assert.equal(cross.status, 401, "cross-Worker context mismatch fails closed");
  assert.equal(pairingEntry(storage)[1].attempts, 1, "a real wrong-code attempt is still counted");

  const okB = await consumePairing(registry, { pairing_id: b.pairing_id, code: b.code });
  assert.equal(okB.response.status, 200, "one context-mismatch attempt does not lock the live session");
});

test("logger redaction never emits raw pairing material, device secrets, or six-digit codes", () => {
  const redacted = sanitize({
    pairing_id: "pair_" + "ab".repeat(32),
    pairing_code: "012345",
    code: "012345",
    device_secret: "devsec_" + "cd".repeat(32),
    nested: { pairing_id: "pair_" + "ef".repeat(32) },
    list: [{ code: "654321" }],
  });
  assert.equal(redacted.pairing_id, "[redacted]");
  assert.equal(redacted.pairing_code, "[redacted]");
  assert.equal(redacted.code, "[redacted]", "bare six-digit code values are redacted");
  assert.equal(redacted.device_secret, "[redacted]");
  assert.equal(redacted.nested.pairing_id, "[redacted]");
  assert.equal(redacted.list[0].code, "[redacted]");

  // Error codes remain loggable: they are never six digits.
  assert.equal(sanitize({ code: "pairing_rejected" }).code, "pairing_rejected");
});

test("no alarm/polling writes exist in the pairing path; authenticate/list/get stay read-only", async () => {
  const { storage, registry } = makeRegistry();
  await createPairing(registry, { ttl_seconds: 60, name: "heartbeat-probe" });
  const { body: session } = await createPairing(registry, { ttl_seconds: 60 });
  const consumed = await consumePairing(registry, { pairing_id: session.pairing_id, code: session.code });
  assert.equal(consumed.response.status, 200);

  const credentialVerifier = await sha256Hex(consumed.body.device_secret);
  const beforeAuth = storage.writeCount;
  const auth = await registry.fetch(new Request("https://registry.internal/internal/devices/authenticate", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ workstation_id: consumed.body.workstation_id, credential_verifier_sha256: credentialVerifier }),
  }));
  assert.equal(auth.status, 200);
  assert.equal(storage.writeCount, beforeAuth, "authenticate must be read-only (no heartbeat writes)");

  await registry.fetch(new Request("https://registry.internal/internal/devices"));
  await registry.fetch(new Request(`https://registry.internal/internal/devices/${consumed.body.device_id}`));
  assert.equal(storage.writeCount, beforeAuth, "list/get must be read-only");
});


test("an unknown pairing_id causes zero storage writes (probe leaves no trace)", async () => {
  const { storage, registry } = makeRegistry();
  await createPairing(registry, { ttl_seconds: 600 });
  const writesBefore = storage.writeCount;

  const unknown = await consumePairing(registry, { pairing_id: `pair_${"b".repeat(64)}`, code: "654321" });
  assert.equal(unknown.response.status, 401);
  assert.equal(unknown.body.code, "pairing_rejected");
  assert.equal(storage.writeCount, writesBefore, "unknown-id probes must not write anything");

  const malformed = await consumePairing(registry, { pairing_id: "garbage", code: "654321" });
  assert.equal(malformed.response.status, 400);
  assert.equal(storage.writeCount, writesBefore, "malformed probes must not write anything");
});

test("externally visible consume failures are uniform across unknown/wrong/expired/locked/consumed", async () => {
  const { storage, registry } = makeRegistry();
  const failures = [];

  const unknown = await consumePairing(registry, { pairing_id: `pair_${"c".repeat(64)}`, code: "111111" });
  failures.push([unknown.response.status, unknown.body.code]);

  const { body: wrong } = await createPairing(registry, { ttl_seconds: 600 });
  const wrongCode = await consumePairing(registry, { pairing_id: wrong.pairing_id, code: "222222" });
  failures.push([wrongCode.response.status, wrongCode.body.code]);

  const { body: expired } = await createPairing(registry, { ttl_seconds: 600 });
  const entry = pairingEntry(storage);
  storage.map.set(entry[0], { ...entry[1], expires_at_ms: Date.now() - 1 });
  const expiredResult = await consumePairing(registry, { pairing_id: expired.pairing_id, code: expired.code });
  failures.push([expiredResult.response.status, expiredResult.body.code]);

  const { body: locked } = await createPairing(registry, { ttl_seconds: 600 });
  for (let i = 0; i < 5; i += 1) {
    await consumePairing(registry, { pairing_id: locked.pairing_id, code: "333333" });
  }
  const lockedResult = await consumePairing(registry, { pairing_id: locked.pairing_id, code: locked.code });
  failures.push([lockedResult.response.status, lockedResult.body.code]);

  const { body: consumed } = await createPairing(registry, { ttl_seconds: 600 });
  const first = await consumePairing(registry, { pairing_id: consumed.pairing_id, code: consumed.code });
  assert.equal(first.response.status, 200);
  const replay = await consumePairing(registry, { pairing_id: consumed.pairing_id, code: consumed.code });
  failures.push([replay.response.status, replay.body.code]);

  for (const [status, code] of failures) {
    assert.deepEqual([status, code], [401, "pairing_rejected"], "every failure mode looks identical outside");
  }
});

test("only one active pairing session exists per Worker; creating a new one atomically replaces the prior", async () => {
  const { storage, registry } = makeRegistry();
  const { body: first } = await createPairing(registry, { ttl_seconds: 600, name: "stale-session" });

  const { body: second } = await createPairing(registry, { ttl_seconds: 600, name: "fresh-session" });
  assert.notEqual(first.pairing_id, second.pairing_id);

  const keys = [...storage.map.keys()].filter((key) => key.startsWith("pairing:"));
  assert.equal(keys.length, 1, "cap of one active unconsumed session is enforced");

  const stale = await consumePairing(registry, { pairing_id: first.pairing_id, code: first.code });
  assert.equal(stale.response.status, 401);
  assert.equal(stale.body.code, "pairing_rejected");

  const fresh = await consumePairing(registry, { pairing_id: second.pairing_id, code: second.code });
  assert.equal(fresh.response.status, 200);
});

test("the six-digit code never travels in a URL; pairing endpoints accept JSON bodies only", async () => {
  const { registry } = makeRegistry();
  const { body: session } = await createPairing(registry, { ttl_seconds: 600 });

  const getCode = await registry.fetch(new Request(
    `https://registry.internal/internal/devices/pairings/consume?pairing_id=${session.pairing_id}&code=${session.code}`,
  ));
  assert.notEqual(getCode.status, 200, "GET/query consumption must not exist");

  const createGet = await registry.fetch(new Request("https://registry.internal/internal/devices/pairings"));
  assert.notEqual(createGet.status, 200);
});

