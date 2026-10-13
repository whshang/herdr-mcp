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
  async getAlarm() { return this.alarmSetAt ?? null; }
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
  return { storage, post, confirm, instance };
}

const snapshot = (storage) => JSON.stringify([...storage.map.entries()]);
// #630 Q4: rejected confirms may append to the per-device reject ring, but
// must never touch grant records or idempotency entries.
const REJECT_PREFIX = "migrate-reject-audit:v1:";
const rejectEntries = (storage, deviceId = DEV) => storage.map.get(`${REJECT_PREFIX}${deviceId}`)?.entries ?? [];
const stateWithoutRejects = (storage) => JSON.stringify([...storage.map.entries()].filter(([key]) => !key.startsWith(REJECT_PREFIX)));
const capturedLogs = async (fn) => {
  const lines = [];
  const original = console.log;
  console.log = (line) => lines.push(line);
  try {
    return { result: await fn(), lines };
  } finally {
    console.log = original;
  }
};
const rejectLogLines = (lines) => lines
  .map((line) => JSON.parse(line))
  .filter((entry) => entry.event === "page_assist_migrate_rejected");

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
  const before = stateWithoutRejects(h.storage);
  const res = await h.confirm({ plan_digest: digest });
  assert.equal(res.status, 409);
  assert.equal(res.body.ok, false);
  assert.equal(res.body.code, "migrate_plan_stale");
  assert.equal(stateWithoutRejects(h.storage), before, "grant + idempotency state untouched");
  assert.equal(h.storage.writes, 1, "only the reject ring wrote");
});

test("user Given a confirm with a stale plan digest When the owner confirms Then the rejection is recorded once in the device reject ring and no grant or idempotency state changes", async () => {
  const h = harness();
  const before = stateWithoutRejects(h.storage);
  const staleDigest = await sha256Hex("not-the-plan");
  const { result: res, lines } = await capturedLogs(() => h.confirm({ plan_digest: staleDigest }));
  assert.equal(res.status, 409);
  assert.deepEqual(res.body, { ok: false, code: "migrate_plan_stale" });
  // The rejection never writes a grant record or an idempotency entry; the only
  // delta is the separate per-device reject ring (one put, no other writes).
  assert.equal(stateWithoutRejects(h.storage), before, "grant + idempotency storage byte-identical");

  const ring = h.storage.map.get(`${REJECT_PREFIX}${DEV}`);
  assert.ok(ring, "per-device reject ring written");
  assert.equal(h.storage.writes, 1, "exactly one reject-ring put wrote");
  assert.equal(ring.v, 1);
  assert.equal(ring.entries.length, 1);
  const entry = ring.entries[0];
  assert.deepEqual(Object.keys(entry).sort(), [
    "at_ms", "code", "device_id", "idempotency_key_sha256_8", "plan_digest", "stage", "v",
  ]);
  assert.equal(entry.v, 1);
  assert.equal(typeof entry.at_ms, "number");
  assert.equal(entry.code, "migrate_plan_stale");
  assert.equal(entry.stage, "digest");
  assert.equal(entry.device_id, DEV);
  assert.equal(entry.plan_digest, staleDigest, "client digest kept when it is lowercase 64-hex");
  assert.equal(entry.idempotency_key_sha256_8, (await sha256Hex(IDEM)).slice(0, 8));
  assert.equal(JSON.stringify(ring).includes(IDEM), false, "raw idempotency key is never stored");

  // One structured reject log line carrying the same fixed fields.
  const logged = rejectLogLines(lines);
  assert.equal(logged.length, 1, "one structured reject log line");
  const fields = logged[0].fields;
  assert.equal(fields.code, "migrate_plan_stale");
  assert.equal(fields.stage, "digest");
  assert.equal(fields.device_id, DEV);
  assert.equal(fields.plan_digest, staleDigest);
  assert.equal(fields.idempotency_key_sha256_8, entry.idempotency_key_sha256_8);
  assert.equal(fields.reject_audit_write_failed, false);
  assert.equal(JSON.stringify(fields).includes(IDEM), false, "raw idempotency key is never logged");
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
  const before = stateWithoutRejects(h.storage);
  const res = await h.confirm({ plan_digest: await sha256Hex("different-plan") });
  assert.equal(res.status, 409);
  assert.equal(res.body.code, "migrate_idempotency_conflict");
  assert.equal(stateWithoutRejects(h.storage), before, "grant + idempotency state untouched");
  assert.equal(h.storage.writes, writesAfterFirst + 1, "conflict writes only the reject ring");
});

test("user Given an injected failure on the write after the grant record When the confirm transaction runs Then everything rolls back and storage is byte-identical", async () => {
  const digest = await planDigest({ from: OLD, to: NEW });
  const h = harness();
  const before = stateWithoutRejects(h.storage);
  h.storage.putFailures = [false, true]; // 1st put (grant) ok, 2nd put (idem/audit) throws
  const res = await h.confirm({ plan_digest: digest });
  assert.equal(res.status, 500, JSON.stringify(res.body));
  assert.equal(res.body.ok, false);
  assert.equal(res.body.code, "migrate_storage_failed");
  assert.equal(stateWithoutRejects(h.storage), before, "grant + idempotency storage byte-identical after rollback");
  // The failed confirm is still audited, after the rolled-back transaction.
  assert.deepEqual(rejectEntries(h.storage).map((entry) => [entry.code, entry.stage]), [["migrate_storage_failed", "storage"]]);
});

test("user Given a postcondition verification failure after the write When the confirm transaction runs Then the transaction rolls back with migrate_postcondition_failed", async () => {
  const digest = await planDigest({ from: OLD, to: NEW });
  const h = harness();
  const before = stateWithoutRejects(h.storage);
  // Corrupt the in-transaction re-read of the grant record after the first
  // write, so post-write verification still sees the unmigrated rows.
  h.storage.readCorruption = { afterWrites: 1 };
  const res = await h.confirm({ plan_digest: digest });
  assert.equal(res.status, 500, JSON.stringify(res.body));
  assert.equal(res.body.ok, false);
  assert.equal(res.body.code, "migrate_postcondition_failed");
  assert.equal(stateWithoutRejects(h.storage), before, "grant + idempotency storage byte-identical after rollback");
  assert.deepEqual(rejectEntries(h.storage).map((entry) => entry.stage), ["postcondition"]);
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
  const before = stateWithoutRejects(h.storage);
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
  assert.equal(stateWithoutRejects(h.storage), before, "grant + idempotency state untouched");
  // The four rejections with an authenticated device are audited; a request with
  // no valid device id is logged only, so it can never create a ring.
  assert.equal(h.storage.writes, 4);
  assert.deepEqual(rejectEntries(h.storage).map((entry) => [entry.code, entry.stage]), [
    ["migrate_idempotency_key_required", "validate"],
    ["migrate_idempotency_key_invalid", "validate"],
    ["migrate_idempotency_key_invalid", "validate"],
    ["migrate_idempotency_key_invalid", "validate"],
  ]);
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
  assert.equal(storage.writes, grantWritesBefore + 1, "only the reject ring wrote");
  assert.deepEqual(rejectEntries(storage).map((entry) => [entry.code, entry.stage]), [["migrate_cross_device_rows", "device_scope"]]);
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

test("user Given a migrated record carrying a success audit entry When allow, deny, or revoke write-backs run Then the audit entry survives each write", async () => {
  const h = harness(record(), {
    [`connector:${NEW}`]: {
      connector_id: NEW, client_id: "c1", status: "active", principal_type: "connector",
      capabilities: ["mcp_access"], resource: "https://issuer/mcp", scope: "mcp",
      redirect_uri: "https://app.example/cb", auth_source: "legacy", grant_generation: 1,
      approved_at_ms: 1000, approved_by: "device:owner",
    },
  });
  const digest = await planDigest({ from: OLD, to: NEW });
  const confirmed = await h.confirm({ plan_digest: digest });
  assert.equal(confirmed.status, 200, JSON.stringify(confirmed.body));
  const auditId = confirmed.body.audit_id;
  const auditSurvives = () => (h.storage.map.get("grant:c1").audit ?? []).some((entry) => entry.audit_id === auditId);
  assert.equal(auditSurvives(), true, "confirm wrote the audit entry");

  const allow = await h.post("/internal/oauth/grant/page-assist", {
    connector_id: NEW, device_id: DEV, endpoint_ref: "ep9", allowed: true, changed_by: `device:${DEV}`,
  });
  assert.equal(allow.status, 200, JSON.stringify(allow.body));
  assert.equal(allow.body.record.page_assist.some((row) => row.endpoint_ref === "ep9" && row.connector_id === NEW), true, "allow applied");
  assert.equal(auditSurvives(), true, "audit survives the allow write-back");

  const deny = await h.post("/internal/oauth/grant/page-assist", {
    connector_id: NEW, device_id: DEV, endpoint_ref: "ep9", allowed: false, changed_by: `device:${DEV}`,
  });
  assert.equal(deny.status, 200, JSON.stringify(deny.body));
  assert.equal(auditSurvives(), true, "audit survives the deny write-back");

  const revoke = await h.post("/internal/oauth/grant/revoke", { client_id: "c1", revoked_by: `device:${DEV}` });
  assert.equal(revoke.status, 200, JSON.stringify(revoke.body));
  assert.equal(h.storage.map.get("grant:c1").status, "revoked", "revoke applied");
  assert.equal(auditSurvives(), true, "audit survives the revoke write-back");
});

test("user Given expired idempotency entries When the DO alarm handler fires Then expired entries are swept and the alarm re-arms while live entries remain", async () => {
  const h = harness();
  h.storage.map.set(`migrate-idem:v1:${DEV}:expired0000000001`, { v: 1, plan_digest: "0".repeat(64), result: { ok: true }, created_at_ms: 1, expires_at_ms: Date.now() - 1000 });
  h.storage.map.set(`migrate-idem:v1:${DEV}:live000000000001`, { v: 1, plan_digest: "0".repeat(64), result: { ok: true }, created_at_ms: 2, expires_at_ms: Date.now() + 60_000 });
  await h.instance.alarm();
  assert.equal(h.storage.map.has(`migrate-idem:v1:${DEV}:expired0000000001`), false, "expired swept");
  assert.equal(h.storage.map.has(`migrate-idem:v1:${DEV}:live000000000001`), true, "live kept");
  assert.ok(h.storage.alarmSetAt !== undefined && h.storage.alarmSetAt > Date.now(), "alarm re-armed");
  // With nothing left to keep, a fully-expired prefix disarms after sweeping.
  h.storage.map.set(`migrate-idem:v1:${DEV}:expired0000000002`, { v: 1, plan_digest: "0".repeat(64), result: { ok: true }, created_at_ms: 3, expires_at_ms: Date.now() - 1 });
  h.storage.alarmSetAt = undefined;
  await h.instance.alarm();
  assert.equal(h.storage.map.has(`migrate-idem:v1:${DEV}:expired0000000002`), false);
  assert.equal(h.storage.map.has(`migrate-idem:v1:${DEV}:live000000000001`), true, "live still kept");
});

test("user Given a confirm with malformed from or to When the DO transaction runs Then it fails closed with 400 and the fixed request-shape code", async () => {
  const h = harness();
  const before = stateWithoutRejects(h.storage);
  const cases = [
    [{ from: "" }, "migrate_from_required"],
    [{ to: "" }, "migrate_to_required"],
    [{ from: "bogus" }, "migrate_invalid_connector_id"],
    [{ from: NEW, to: NEW }, "migrate_source_is_current"],
  ];
  for (const [over, code] of cases) {
    const res = await h.confirm({ plan_digest: "a".repeat(64), ...over });
    assert.equal(res.status, 400, JSON.stringify(res.body));
    assert.equal(res.body.code, code);
  }
  assert.equal(stateWithoutRejects(h.storage), before, "grant + idempotency state untouched");
  assert.equal(h.storage.writes, 4, "one reject ring entry per rejected confirm");
});

test("user Given a successful confirm When the idempotency entry is written Then an alarm is already scheduled and firing it sweeps the expired entry", async () => {
  const h = harness();
  const digest = await planDigest({ from: OLD, to: NEW });
  const res = await h.confirm({ plan_digest: digest });
  assert.equal(res.status, 200, JSON.stringify(res.body));
  // B1: the confirm itself must first-schedule the alarm (the sweep can only
  // re-arm an alarm that exists).
  assert.ok(Number.isFinite(h.storage.alarmSetAt), `alarm scheduled by confirm (got ${h.storage.alarmSetAt})`);
  assert.ok(h.storage.alarmSetAt > Date.now(), "alarm is in the future");
  const writesAfterConfirm = h.storage.writes;
  // Age the entry past its TTL and fire the scheduled handler.
  const idemKey = `migrate-idem:v1:${DEV}:${IDEM}`;
  const aged = h.storage.map.get(idemKey);
  aged.expires_at_ms = Date.now() - 1;
  h.storage.map.set(idemKey, aged);
  await h.instance.alarm();
  assert.equal(h.storage.map.has(idemKey), false, "expired entry swept");
  assert.ok(h.storage.alarmSetAt !== undefined, "alarm re-armed while entries remain");
  assert.equal(h.storage.writes, writesAfterConfirm + 1, "exactly the sweep delete wrote");
});

test("user Given a replay while an alarm is already pending When the stored result is returned Then the existing alarm is not rescheduled", async () => {
  const h = harness();
  const digest = await planDigest({ from: OLD, to: NEW });
  assert.equal((await h.confirm({ plan_digest: digest })).status, 200);
  const firstAlarm = h.storage.alarmSetAt;
  assert.ok(Number.isFinite(firstAlarm));
  assert.equal((await h.confirm({ plan_digest: digest })).status, 200);
  assert.equal(h.storage.alarmSetAt, firstAlarm, "pending alarm untouched");
});

test("user Given a reject ring at its cap and a failing reject write When confirms are rejected Then the ring evicts oldest per device and the original rejection is returned", async () => {
  const staleDigest = await sha256Hex("stale");

  // Cap 64 per device, oldest evicted first.
  const h = harness();
  const prefill = [];
  for (let i = 1; i <= 64; i += 1) {
    prefill.push({ v: 1, at_ms: i, code: "migrate_plan_stale", stage: "digest", device_id: DEV, plan_digest: null, idempotency_key_sha256_8: null });
  }
  h.storage.map.set(`${REJECT_PREFIX}${DEV}`, { v: 1, entries: prefill });
  assert.equal((await h.confirm({ plan_digest: staleDigest })).status, 409);
  const capped = rejectEntries(h.storage);
  assert.equal(capped.length, 64, "cap holds 64 entries");
  assert.equal(capped[0].at_ms, 2, "oldest entry evicted first");
  assert.equal(capped[63].at_ms > 0, true, "newest entry kept");
  assert.equal(capped[63].code, "migrate_plan_stale");

  // Per-device isolation: another device's rejection goes to its own ring.
  const hOther = harness();
  const other = await hOther.confirm({ plan_digest: staleDigest, device_id: OTHER });
  assert.equal(other.status, 409);
  assert.equal(other.body.code, "migrate_cross_device_rows");
  assert.equal(hOther.storage.map.has(`${REJECT_PREFIX}${DEV}`), false, "no entry under the other device's ring");
  assert.deepEqual(rejectEntries(hOther.storage, OTHER).map((entry) => [entry.device_id, entry.stage]), [[OTHER, "device_scope"]]);

  // A failing reject-ring write keeps the original rejection and only flags it.
  const hFail = harness();
  const before = stateWithoutRejects(hFail.storage);
  hFail.storage.putFailures = [true];
  const { result: failed, lines } = await capturedLogs(() => hFail.confirm({ plan_digest: staleDigest }));
  assert.equal(failed.status, 409);
  assert.deepEqual(failed.body, { ok: false, code: "migrate_plan_stale" });
  assert.equal(stateWithoutRejects(hFail.storage), before, "a failed reject write never touches grant or idempotency state");
  assert.equal(hFail.storage.map.has(`${REJECT_PREFIX}${DEV}`), false, "no partial ring written");
  const [logged] = rejectLogLines(lines);
  assert.equal(logged.fields.reject_audit_write_failed, true);
  assert.equal(logged.fields.code, "migrate_plan_stale");
  assert.equal(logged.fields.stage, "digest");
  assert.equal(logged.fields.idempotency_key_sha256_8, (await sha256Hex(IDEM)).slice(0, 8));
});
