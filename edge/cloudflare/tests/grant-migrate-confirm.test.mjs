// #630 first commit: DO-internal transaction core for `connector page-assist
// migrate --confirm`. Tests drive the real OAuthStoreDO through its internal
// HTTP route with a transaction-capable StorageMock (rollback = throw inside
// storage.transaction discards every buffered write).
import { test } from "node:test";
import assert from "node:assert/strict";
import { OAuthStoreDO } from "../dist/oauth-store-do.js";
import { sha256Hex } from "../dist/device-crypto.js";

const ROUTE = "/internal/oauth/grant/page-assist-migrate";

class StorageMock {
  map = new Map();
  writes = 0;
  alarmSetAt = undefined;
  async setAlarm(at) { this.alarmSetAt = at; }
  putFailures = []; // queued failures injected on the Nth put
  readCorruption = null; // { afterWrites } -> post-write reads return pre-write bytes
  async get(key) { return structuredClone(this.map.get(key)); }
  async put(key, value) {
    if (this.putFailures.length > 0) {
      const fail = this.putFailures.shift();
      if (fail) throw new Error("injected put failure");
    }
    this.writes++;
    this.map.set(key, structuredClone(value));
  }
  async delete(key) { this.writes++; return this.map.delete(key); }
  async list(opts = {}) {
    const out = new Map();
    for (const [k, v] of this.map) if (k.startsWith(opts.prefix ?? "")) out.set(k, structuredClone(v));
    return out;
  }
  // Real DO semantics: writes inside the closure are buffered and only applied
  // when it resolves. A throw discards the whole buffer.
  async transaction(fn) {
    const buffered = new Map();
    const deleted = new Set();
    const tx = {
      get: async (key) => {
        const value = this.map.get(key);
        if (value === undefined) return undefined;
        // Optional test hook: once N writes happened in the transaction, reads
        // observe the pre-write bytes even when a put was buffered — the
        // postcondition must catch this and roll the transaction back.
        if (this.readCorruption && this.writes >= this.readCorruption.afterWrites) {
          return structuredClone(value);
        }
        return structuredClone(buffered.has(key) ? buffered.get(key) : value);
      },
      put: async (key, value) => {
        if (this.putFailures.length > 0) {
          const fail = this.putFailures.shift();
          if (fail) throw new Error("injected put failure");
        }
        this.writes++;
        buffered.set(key, structuredClone(value));
        deleted.delete(key);
      },
      delete: async (key) => { this.writes++; buffered.delete(key); deleted.add(key); },
      list: async (o = {}) => {
        const out = new Map();
        for (const [k, v] of this.map) {
          if (!k.startsWith(o.prefix ?? "")) continue;
          if (deleted.has(k)) continue;
          out.set(k, structuredClone(buffered.has(k) ? buffered.get(k) : v));
        }
        return out;
      },
    };
    await fn(tx);
    for (const key of deleted) this.map.delete(key);
    for (const [key, value] of buffered) this.map.set(key, value);
  }
}

const OLD = "conn_old00000001";
const OLD2 = "conn_old00000002";
const NEW = "conn_new00000001";
const DEV = "dev_01HZZZZZZZZZZZZZZZZZZZZZZZ";
const OTHER = "dev_01HYYYYYYYYYYYYYYYYYYYYYYY";
const IDEM = "idem-key-1234567890abcdef";
const pa = (connector_id, endpoint_ref, device_id = DEV) => ({ connector_id, grant_generation: 1, device_id, endpoint_ref });

function record(overrides = {}) {
  return {
    client_id: "c1",
    status: "active",
    principal_type: "connector",
    resource: "https://issuer/mcp",
    scope: "mcp",
    approved_at_ms: 1000,
    approved_by: "device:owner",
    connector_id: NEW,
    grant_generation: 1,
    page_assist: [pa(OLD, "ep1"), pa(NEW, "ep2"), pa(OLD2, "ep3")],
    webchat_control: [{ ...pa(OLD, "ep4"), provider: "chatgpt", account_ref: "*" }],
    ...overrides,
  };
}

async function planDigest(body) {
  // Spec A: the dry run is device-scoped and returns the filtered plan plus
  // its digest. The test consumes exactly what the server returned.
  const h = harness();
  const plan = await h.post("/internal/oauth/grant/page-assist-orphans", { action: "plan", from: body.from, to: body.to, device_id: DEV });
  assert.equal(plan.body.ok, true, "dry-run plan for digest");
  assert.equal(typeof plan.body.plan_digest, "string", "dry run returns plan_digest");
  assert.ok(/^[0-9a-f]{64}$/.test(plan.body.plan_digest), "digest is sha256 hex");
  return plan.body.plan_digest;
}

function harness(rec = record(), extra = {}) {
  const storage = new StorageMock();
  storage.map.set("grant:c1", structuredClone(rec));
  for (const [key, value] of Object.entries(extra)) storage.map.set(key, structuredClone(value));
  const instance = new OAuthStoreDO({ storage }, { OAUTH_ISSUER: "https://issuer" });
  const post = async (path, body) => {
    const resp = await instance.fetch(new Request(`https://do.internal${path}`, {
      method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body),
    }));
    return { status: resp.status, body: await resp.json() };
  };
  const confirm = (over = {}) => post(ROUTE, {
    from: OLD, to: NEW, device_id: DEV, idempotency_key: IDEM, ...over,
  });
  return { storage, post, confirm };
}

const snapshot = (storage) => JSON.stringify([...storage.map.entries()]);

test("user Given a fresh dry-run plan When the owner confirms the migrate Then only planned rows move, an audit entry and idempotency entry are written in one transaction", async () => {
  const h = harness();
  const digest = await planDigest({ from: OLD, to: NEW });
  const before = snapshot(h.storage);
  const res = await h.confirm({ plan_digest: digest });
  assert.equal(res.status, 200, JSON.stringify(res.body));
  assert.equal(res.body.ok, true);
  assert.equal(res.body.action, "connector_page_assist_migrated");
  assert.equal(res.body.moved, 2);
  assert.equal(typeof res.body.audit_id, "string");

  const stored = h.storage.map.get("grant:c1");
  assert.equal(stored.page_assist[0].connector_id, NEW, "ep1 moved");
  assert.equal(stored.page_assist[1].connector_id, NEW, "ep2 untouched");
  assert.equal(stored.page_assist[2].connector_id, OLD2, "OLD2 rows untouched");
  assert.equal(stored.webchat_control[0].connector_id, NEW, "ep4 moved");
  // Success audit appended in the same transaction: action migrate, by device.
  const audits = (stored.audit ?? []).filter((a) => a.action === "migrate");
  assert.equal(audits.length, 1);
  assert.equal(audits[0].by, `device:${DEV}`);
  assert.equal(audits[0].from_connector_id, OLD);
  assert.equal(audits[0].to_connector_id, NEW);
  assert.deepEqual(audits[0].rows.map((r) => [r.device_id, r.endpoint_ref]), [[DEV, "ep1"], [DEV, "ep4"]]);
  // Idempotency entry stored under the device-scoped key.
  const idem = h.storage.map.get(`migrate-idem:v1:${DEV}:${IDEM}`);
  assert.ok(idem, "idempotency entry written");
  assert.equal(idem.plan_digest, digest);
  assert.equal(idem.result.action, "connector_page_assist_migrated");
  assert.equal(idem.result.moved, 2);
  assert.equal(idem.result.audit_id, audits[0].audit_id);
  assert.ok(h.storage.writes >= 2, `grant + idem writes happened (${h.storage.writes})`);
  assert.notEqual(snapshot(h.storage), before);
});

test("user Given a plan whose target record changed before confirm When the owner confirms Then it fails closed with migrate_plan_stale and writes nothing", async () => {
  // Build the digest for the original state, then age the record.
  const digest = await planDigest({ from: OLD, to: NEW });
  const stale = record({ grant_generation: 2 });
  const h = harness(stale);
  const before = snapshot(h.storage);
  const res = await h.confirm({ plan_digest: digest });
  assert.equal(res.status, 409);
  assert.equal(res.body.ok, false);
  assert.equal(res.body.code, "migrate_plan_stale");
  assert.equal(snapshot(h.storage), before);
  assert.equal(h.storage.writes, 0);
});

test("user Given a confirm with the wrong plan_digest When the owner confirms Then it fails closed with migrate_plan_stale and writes nothing", async () => {
  const h = harness();
  const before = snapshot(h.storage);
  const res = await h.confirm({ plan_digest: await sha256Hex("not-the-plan") });
  assert.equal(res.status, 409);
  assert.deepEqual(res.body, { ok: false, code: "migrate_plan_stale" });
  assert.equal(snapshot(h.storage), before);
  assert.equal(h.storage.writes, 0);
});

test("user Given a successful confirm When the same idempotency key and digest are replayed Then the stored result is returned with zero writes", async () => {
  const h = harness();
  const digest = await planDigest({ from: OLD, to: NEW });
  const first = await h.confirm({ plan_digest: digest });
  assert.equal(first.status, 200);
  const writesAfterFirst = h.storage.writes;
  const afterFirst = snapshot(h.storage);
  const replay = await h.confirm({ plan_digest: digest });
  assert.equal(replay.status, 200);
  assert.deepEqual(replay.body, first.body);
  assert.equal(h.storage.writes, writesAfterFirst, "replay performs no writes");
  assert.equal(snapshot(h.storage), afterFirst);
});

test("user Given a stored idempotency entry When the same key is reused with a different plan digest Then it fails closed with 409 migrate_idempotency_conflict", async () => {
  const h = harness();
  const digest = await planDigest({ from: OLD, to: NEW });
  const first = await h.confirm({ plan_digest: digest });
  assert.equal(first.status, 200);
  const writesAfterFirst = h.storage.writes;
  const before = snapshot(h.storage);
  const res = await h.confirm({ plan_digest: await sha256Hex("different-plan") });
  assert.equal(res.status, 409);
  assert.equal(res.body.code, "migrate_idempotency_conflict");
  assert.equal(snapshot(h.storage), before);
  assert.equal(h.storage.writes, writesAfterFirst, "conflict performs no writes");
});

test("user Given an injected failure on the write after the grant record When the confirm transaction runs Then everything rolls back and storage is byte-identical", async () => {
  const digest = await planDigest({ from: OLD, to: NEW });
  const h = harness();
  const before = snapshot(h.storage);
  h.storage.putFailures = [false, true]; // 1st put (grant) ok, 2nd put (idem/audit) throws
  const res = await h.confirm({ plan_digest: digest });
  assert.equal(res.status, 500, JSON.stringify(res.body));
  assert.equal(res.body.ok, false);
  assert.equal(res.body.code, "migrate_storage_failed");
  assert.equal(snapshot(h.storage), before, "storage byte-identical after rollback");
});

test("user Given a postcondition verification failure after the write When the confirm transaction runs Then the transaction rolls back with migrate_postcondition_failed", async () => {
  const digest = await planDigest({ from: OLD, to: NEW });
  const h = harness();
  const before = snapshot(h.storage);
  // Corrupt the in-transaction re-read of the grant record after the first
  // write, so post-write verification still sees the unmigrated rows.
  h.storage.readCorruption = { afterWrites: 1 };
  const res = await h.confirm({ plan_digest: digest });
  assert.equal(res.status, 500, JSON.stringify(res.body));
  assert.equal(res.body.ok, false);
  assert.equal(res.body.code, "migrate_postcondition_failed");
  assert.equal(snapshot(h.storage), before, "storage byte-identical after rollback");
});

test("user Given matching rows of two devices When the owner runs a device-scoped dry run Then the plan itself fails closed with migrate_cross_device_rows and nothing is written", async () => {
  const rec = record();
  rec.page_assist.push(pa(OLD, "ep5", OTHER));
  const storage = new StorageMock();
  storage.map.set("grant:c1", structuredClone(rec));
  const instance = new OAuthStoreDO({ storage }, { OAUTH_ISSUER: "https://issuer" });
  const before = snapshot(storage);
  const resp = await instance.fetch(new Request(`https://do.internal/internal/oauth/grant/page-assist-orphans`, {
    method: "POST", headers: { "content-type": "application/json" },
    body: JSON.stringify({ action: "plan", from: OLD, to: NEW, device_id: DEV }),
  }));
  const res = { status: resp.status, body: await resp.json() };
  assert.equal(res.status, 409);
  assert.equal(res.body.code, "migrate_cross_device_rows");
  assert.equal(snapshot(storage), before);
  assert.equal(storage.writes, 0);
});

test("user Given a confirm without an idempotency key or with a malformed one When the owner confirms Then it fails closed with 400 and writes nothing", async () => {
  const h = harness();
  const before = snapshot(h.storage);
  const missing = await h.confirm({ plan_digest: await sha256Hex("x"), idempotency_key: undefined });
  assert.equal(missing.status, 400);
  assert.equal(missing.body.code, "migrate_idempotency_key_required");
  for (const key of ["short", "has spaces and !", "x".repeat(65)]) {
    const bad = await h.confirm({ plan_digest: await sha256Hex("x"), idempotency_key: key });
    assert.equal(bad.status, 400, JSON.stringify(key));
    assert.equal(bad.body.code, "migrate_idempotency_key_invalid");
  }
  const badDevice = await h.confirm({ plan_digest: await sha256Hex("x"), device_id: "bogus" });
  assert.equal(badDevice.status, 400);
  assert.equal(snapshot(h.storage), before);
  assert.equal(h.storage.writes, 0);
});

test("user Given an unfiltered digest computed over a plan that includes another device's rows When the owner confirms Then the cross-device check rejects first with zero grant writes", async () => {
  const rec = record();
  rec.page_assist.push(pa(OLD, "ep5", OTHER));
  const storage = new StorageMock();
  storage.map.set("grant:c1", structuredClone(rec));
  const instance = new OAuthStoreDO({ storage }, { OAUTH_ISSUER: "https://issuer" });
  // The client (pre-spec-A server, or a hand-rolled caller) computes a digest
  // over the UNFILTERED plan, including OTHER's ep5 row. Spec A requires the
  // confirm path's cross-device rejection to fire regardless of the digest.
  const sortKey = (m) => m.kind + m.from.device_id + m.from.endpoint_ref + (m.from.provider ?? "");
  const ownRows = [pa(OLD, "ep1"), { ...pa(OLD, "ep4"), provider: "chatgpt", account_ref: "*" }]
    .map((row) => ({ kind: row.provider ? "webchat_control" : "page_assist", from: { ...row, client_id: "c1", current_connector_id: NEW }, to: {} }));
  const unfiltered = { from_connector_id: OLD, to_connector_id: NEW, grant_generation: 1, moves: [
    ...ownRows,
    { kind: "page_assist", from: { ...pa(OLD, "ep5", OTHER), client_id: "c1", current_connector_id: NEW }, to: {} },
  ].sort((a, b) => sortKey(a).localeCompare(sortKey(b))) };
  const digest = await sha256Hex(JSON.stringify(unfiltered));
  const grantWritesBefore = storage.writes;
  const resp = await instance.fetch(new Request(`https://do.internal${ROUTE}`, {
    method: "POST", headers: { "content-type": "application/json" },
    body: JSON.stringify({ from: OLD, to: NEW, plan_digest: digest, idempotency_key: IDEM, device_id: DEV, by: `device:${DEV}` }),
  }));
  const res = { status: resp.status, body: await resp.json() };
  assert.equal(res.status, 409, JSON.stringify(res.body));
  assert.equal(res.body.code, "migrate_cross_device_rows");
  assert.equal(storage.writes, grantWritesBefore, "zero writes after rejection");
  const stored = storage.map.get("grant:c1");
  assert.equal(stored.page_assist.find((row) => row.endpoint_ref === "ep5").connector_id, OLD, "other device row untouched");
});

test("user Given an expired idempotency entry When the same key is replayed Then the entry is treated as absent, deleted, and the request re-plans", async () => {
  const h = harness();
  const digest = await planDigest({ from: OLD, to: NEW });
  const first = await h.confirm({ plan_digest: digest });
  assert.equal(first.status, 200);
  // Age the stored entry past its TTL.
  const idemKey = `migrate-idem:v1:${DEV}:${IDEM}`;
  const aged = h.storage.map.get(idemKey);
  aged.expires_at_ms = Date.now() - 1;
  h.storage.map.set(idemKey, aged);
  // The rows already moved: a re-plan now fails closed with a stable code
  // (no matching rows under OLD anymore) instead of returning the stale result.
  const res = await h.confirm({ plan_digest: digest });
  assert.equal(res.status, 409);
  assert.equal(res.body.code, "migrate_no_matching_rows");
  assert.equal(h.storage.map.has(idemKey), false, "expired entry deleted in the same transaction");
});

test("user Given a device at the 256-entry idempotency cap When a new confirm succeeds Then the oldest entry is evicted and the new one stored", async () => {
  const h = harness();
  // Pre-fill 256 entries, oldest first.
  const oldestKey = `migrate-idem:v1:${DEV}:prefill000000000001`;
  for (let i = 1; i <= 256; i += 1) {
    h.storage.map.set(`migrate-idem:v1:${DEV}:prefill${String(i).padStart(12, "0")}`, {
      v: 1, plan_digest: "0".repeat(64), result: { ok: true }, created_at_ms: i, expires_at_ms: Date.now() + 60_000,
    });
  }
  const digest = await planDigest({ from: OLD, to: NEW });
  const res = await h.confirm({ plan_digest: digest });
  assert.equal(res.status, 200, JSON.stringify(res.body));
  assert.equal(h.storage.map.has(oldestKey), false, "oldest entry evicted at cap");
  assert.notEqual(h.storage.map.has(`migrate-idem:v1:${DEV}:${IDEM}`), false, "new entry stored");
  let count = 0;
  for (const key of h.storage.map.keys()) if (key.startsWith(`migrate-idem:v1:${DEV}:`)) count += 1;
  assert.equal(count, 256, "cap still holds 256 entries");
});

test("user Given expired idempotency entries When the alarm sweep runs Then expired entries are deleted and the alarm stays armed while any remain", async () => {
  const h = harness();
  h.storage.map.set(`migrate-idem:v1:${DEV}:expired0000000001`, { v: 1, plan_digest: "0".repeat(64), result: { ok: true }, created_at_ms: 1, expires_at_ms: Date.now() - 1000 });
  h.storage.map.set(`migrate-idem:v1:${DEV}:live000000000001`, { v: 1, plan_digest: "0".repeat(64), result: { ok: true }, created_at_ms: 2, expires_at_ms: Date.now() + 60_000 });
  const res = await h.post("/internal/oauth/grant/page-assist-migrate-alarm", {});
  assert.equal(res.status, 200);
  assert.equal(res.body.ok, true);
  assert.equal(res.body.deleted, 1);
  assert.equal(h.storage.map.has(`migrate-idem:v1:${DEV}:expired0000000001`), false, "expired deleted");
  assert.equal(h.storage.map.has(`migrate-idem:v1:${DEV}:live000000000001`), true, "live kept");
  assert.ok(h.storage.alarmSetAt !== undefined, "alarm re-armed while live entries remain");
});

test("user Given a revoked grant record after a successful confirm When the same key is replayed Then the stored result is returned and any new key fails closed", async () => {
  const h = harness();
  const digest = await planDigest({ from: OLD, to: NEW });
  const first = await h.confirm({ plan_digest: digest });
  assert.equal(first.status, 200);
  // Revoke the record after the successful confirm.
  const revoked = h.storage.map.get("grant:c1");
  revoked.status = "revoked";
  revoked.revoked_at_ms = Date.now();
  h.storage.map.set("grant:c1", revoked);
  const writesBeforeReplay = h.storage.writes;
  const replay = await h.confirm({ plan_digest: digest });
  assert.equal(replay.status, 200, "stored past result replayed");
  assert.deepEqual(replay.body, first.body);
  assert.equal(h.storage.writes, writesBeforeReplay, "replay grants nothing and writes nothing");
  const newKey = await h.confirm({ plan_digest: digest, idempotency_key: "fresh-key-000000000001" });
  assert.equal(newKey.status, 409);
  assert.equal(newKey.body.code, "grant_record_not_active", "a new key re-plans and fails closed");
});
