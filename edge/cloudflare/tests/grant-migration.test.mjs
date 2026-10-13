import { test } from "node:test";
import assert from "node:assert/strict";
import { OAuthStoreDO } from "../dist/oauth-store-do.js";
import { listOrphans, planMigration, applyPlanToRawRecord } from "../dist/grant-migration.js";

class StorageMock {
  map = new Map();
  writes = 0;
  async get(key) { return structuredClone(this.map.get(key)); }
  async put(key, value) { this.writes++; this.map.set(key, structuredClone(value)); }
  async delete(key) { this.writes++; return this.map.delete(key); }
  async list(opts = {}) {
    const out = new Map();
    for (const [k, v] of this.map) if (k.startsWith(opts.prefix ?? "")) out.set(k, structuredClone(v));
    return out;
  }
  async transaction(fn) { return fn(this); }
}

const OLD = "conn_old00000001";
const OLD2 = "conn_old00000002";
const NEW = "conn_new00000001";
const DEV = "dev_01HZZZZZZZZZZZZZZZZZZZZZZZ";
const pa = (connector_id, endpoint_ref) => ({ connector_id, grant_generation: 1, device_id: DEV, endpoint_ref });

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

function harness(rec = record()) {
  const storage = new StorageMock();
  storage.map.set("grant:c1", structuredClone(rec));
  const instance = new OAuthStoreDO({ storage }, { OAUTH_ISSUER: "https://issuer" });
  const post = async (body) => {
    const resp = await instance.fetch(new Request("https://do.internal/internal/oauth/grant/page-assist-orphans", {
      method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body),
    }));
    return { status: resp.status, body: await resp.json() };
  };
  return { storage, post };
}

const snapshot = (storage) => JSON.stringify([...storage.map.entries()]);

test("user Given a grant record with carried-forward rows When the owner lists orphans Then only rows not under the current connector are returned and nothing is written", async () => {
  const h = harness();
  const before = snapshot(h.storage);
  const res = await h.post({ action: "list" });
  assert.equal(res.status, 200);
  assert.equal(res.body.ok, true);
  assert.deepEqual(res.body.orphans.map((o) => [o.kind, o.connector_id, o.endpoint_ref]), [
    ["page_assist", OLD, "ep1"], ["page_assist", OLD2, "ep3"], ["webchat_control", OLD, "ep4"],
  ]);
  assert.ok(res.body.orphans.every((o) => o.current_connector_id === NEW));
  assert.equal(snapshot(h.storage), before);
  assert.equal(h.storage.writes, 0);
});

test("user Given orphan rows under --from When the owner runs a migrate dry run Then only --from rows are planned and storage stays byte-identical", async () => {
  const h = harness();
  const before = snapshot(h.storage);
  const res = await h.post({ action: "plan", from: OLD, to: NEW });
  assert.equal(res.status, 200);
  assert.equal(res.body.dry_run, true);
  assert.equal(res.body.moves.length, 2);
  for (const m of res.body.moves) {
    assert.equal(m.from.connector_id, OLD);
    assert.equal(m.to.connector_id, NEW);
    assert.equal(m.to.grant_generation, 1);
    assert.equal(m.to.endpoint_ref, m.from.endpoint_ref);
    assert.equal(m.to.device_id, m.from.device_id);
  }
  assert.equal(snapshot(h.storage), before);
  assert.equal(h.storage.writes, 0);
});

test("user Given missing or ambiguous selectors When the owner runs a migrate dry run Then it fails closed with a stable code and no writes", async () => {
  const h = harness();
  const before = snapshot(h.storage);
  const cases = [
    [{ action: "plan", to: NEW }, "migrate_from_required"],
    [{ action: "plan", from: OLD }, "migrate_to_required"],
    [{ action: "plan", from: "", to: NEW }, "migrate_from_required"],
    [{ action: "plan", from: "bogus", to: NEW }, "migrate_invalid_connector_id"],
    [{ action: "plan", from: NEW, to: NEW }, "migrate_source_is_current"],
    [{ action: "plan", from: OLD, to: OLD2 }, "migrate_target_not_current"],
    [{ action: "plan", from: "conn_unknown0001", to: NEW }, "migrate_no_matching_rows"],
  ];
  for (const [input, code] of cases) {
    const res = await h.post(input);
    assert.equal(res.status, 409, JSON.stringify(input));
    assert.deepEqual(res.body, { ok: false, code });
  }
  assert.equal((await h.post({ action: "apply", from: OLD, to: NEW })).status, 400);
  assert.equal(snapshot(h.storage), before);
  assert.equal(h.storage.writes, 0);
});

test("user Given two records claiming the same current connector When a migration is planned Then it fails closed as ambiguous", () => {
  const res = planMigration([record(), record({ client_id: "c2" })], OLD, NEW);
  assert.deepEqual(res, { ok: false, code: "migrate_target_ambiguous" });
});

test("user Given a revoked grant record When the owner lists orphans or plans a migration Then nothing is offered", async () => {
  const h = harness(record({ status: "revoked" }));
  const list = await h.post({ action: "list" });
  assert.deepEqual(list.body, { ok: true, orphans: [], invalid_records: 0 });
  const plan = await h.post({ action: "plan", from: OLD, to: NEW });
  assert.equal(plan.body.ok, false);
  assert.equal(h.storage.writes, 0);
  assert.deepEqual(listOrphans(record({ status: "revoked" })), []);
  assert.deepEqual(listOrphans(null), []);
});

test("user Given the target connector already holds the same device and endpoint When a migration is planned Then it fails closed with migrate_target_conflict and writes nothing", async () => {
  const rec = record();
  rec.page_assist.push(pa(NEW, "ep1")); // target already holds the ep1 destination
  const h = harness(rec);
  const before = snapshot(h.storage);
  const res = await h.post({ action: "plan", from: OLD, to: NEW });
  assert.equal(res.status, 409);
  assert.equal(res.body.ok, false);
  assert.equal(res.body.code, "migrate_target_conflict");
  assert.deepEqual(res.body.conflicts.map((c) => [c.kind, c.endpoint_ref]), [["page_assist", "ep1"]]);
  assert.equal(snapshot(h.storage), before);
  assert.equal(h.storage.writes, 0);
  // Non-conflicting different endpoint on target is fine.
  const ok = planMigration([record({ page_assist: [pa(OLD, "ep1"), pa(NEW, "ep9")] })], OLD, NEW);
  assert.equal(ok.ok, true);
});

test("user Given two source rows that land on the same destination When a migration is planned Then it fails closed with migrate_target_conflict", () => {
  const dup = { ...pa(OLD, "ep1"), grant_generation: 2 };
  const res = planMigration([record({ page_assist: [pa(OLD, "ep1"), dup], webchat_control: [] })], OLD, NEW);
  assert.equal(res.code, "migrate_target_conflict");
});

test("user Given a stored record with unknown fields When the plan is applied to the raw record Then unknown fields are preserved and the input is not mutated", () => {
  const raw = {
    ...record(),
    audit: [{ at_ms: 1, by: "device:x", action: "allow" }],
    future_field: { nested: true },
  };
  raw.page_assist[0] = { ...raw.page_assist[0], note: "keep-me" };
  const frozen = JSON.stringify(raw);
  const plan = planMigration([record()], OLD, NEW);
  assert.equal(plan.ok, true);
  const next = applyPlanToRawRecord(raw, plan);
  assert.equal(JSON.stringify(raw), frozen, "input unchanged");
  assert.deepEqual(next.audit, raw.audit);
  assert.deepEqual(next.future_field, raw.future_field);
  assert.equal(next.page_assist[0].note, "keep-me");
  assert.equal(next.page_assist[0].connector_id, NEW);
  assert.equal(next.page_assist[2].connector_id, OLD2, "rows of other connectors untouched");
  assert.equal(next.webchat_control[0].connector_id, NEW);
  assert.equal(next.webchat_control[0].provider, "chatgpt");
  // Everything except the rewritten rows is identical.
  const strip = (r) => ({ ...r, page_assist: undefined, webchat_control: undefined });
  assert.deepEqual(strip(next), strip(raw));
});

test("user Given records with missing fields When the owner lists or plans Then a fixed code is returned and invalid records are counted, never offered", async () => {
  const rec = record();
  delete rec.grant_generation;
  assert.deepEqual(planMigration([rec], OLD, NEW), { ok: false, code: "grant_record_missing_field" });
  const h = harness(record());
  h.storage.map.set("grant:broken", { client_id: "broken", status: "active" }); // missing approved_* -> invalid
  const before = snapshot(h.storage);
  const res = await h.post({ action: "list" });
  assert.equal(res.body.invalid_records, 1);
  assert.ok(res.body.orphans.every((o) => o.client_id === "c1"));
  assert.equal(snapshot(h.storage), before);
  assert.equal(h.storage.writes, 0);
});
