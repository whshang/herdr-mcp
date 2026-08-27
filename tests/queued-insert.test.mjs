import assert from "node:assert/strict";
import test from "node:test";
import {
  QUEUED_INSERT_MAX_ITEMS,
  ackQueuedInsertBatch,
  clearQueuedInserts,
  enqueueQueuedInsert,
  moveQueuedInserts,
  normalizeQueuedInsertState,
  queuedInsertBatch,
  queuedInsertStatus,
} from "../extension/queued-insert-core.js";

const CONV = "https://chatgpt.com/c/example";
const BASE = Date.now();

function enqueue(state, text, index) {
  return enqueueQueuedInsert(state, CONV, text, { now: BASE + index, id: `q${index}` });
}

test("queued inserts preserve order and merge into one next-turn message", () => {
  let state = normalizeQueuedInsertState({});
  for (const [index, text] of ["check the API", "also run the smoke test", "do not publish yet"].entries()) {
    const result = enqueue(state, text, index);
    assert.equal(result.ok, true);
    state = result.state;
  }
  const batch = queuedInsertBatch(state, CONV);
  assert.deepEqual(batch.entry_ids, ["q0", "q1", "q2"]);
  assert.equal(batch.text, "check the API\n\nalso run the smoke test\n\ndo not publish yet");
  assert.equal(batch.count, 3);
});

test("ack removes only the delivered batch so concurrent enqueues survive", () => {
  let state = enqueue(normalizeQueuedInsertState({}), "first", 0).state;
  const batch = queuedInsertBatch(state, CONV);
  state = enqueue(state, "arrived while sending", 1).state;
  state = ackQueuedInsertBatch(state, CONV, batch.entry_ids);
  const next = queuedInsertBatch(state, CONV);
  assert.equal(next.count, 1);
  assert.equal(next.text, "arrived while sending");
});

test("queue is bounded and rejects overflow without dropping existing messages", () => {
  let state = normalizeQueuedInsertState({});
  for (let i = 0; i < QUEUED_INSERT_MAX_ITEMS; i += 1) {
    const result = enqueue(state, `message ${i}`, i);
    assert.equal(result.ok, true);
    state = result.state;
  }
  const rejected = enqueue(state, "one too many", 99);
  assert.equal(rejected.ok, false);
  assert.equal(rejected.error, "queue-full");
  assert.equal(queuedInsertStatus(rejected.state, CONV).count, QUEUED_INSERT_MAX_ITEMS);
});

test("normalization prunes malformed, duplicate, and expired rows", () => {
  const now = 2_000_000_000_000;
  const state = normalizeQueuedInsertState({
    conversations: {
      [CONV]: [
        { id: "good", text: "keep", created_at: now - 1000 },
        { id: "good", text: "duplicate", created_at: now - 900 },
        { id: "old", text: "expired", created_at: now - 8 * 24 * 60 * 60 * 1000 },
        { id: "", text: "bad", created_at: now },
      ],
    },
  }, now);
  assert.deepEqual(state.conversations[CONV], [{ id: "good", text: "keep", created_at: now - 1000 }]);
});

test("clear removes one conversation queue", () => {
  let state = enqueue(normalizeQueuedInsertState({}), "first", 0).state;
  state = clearQueuedInserts(state, CONV);
  assert.equal(queuedInsertStatus(state, CONV).count, 0);
  assert.equal(queuedInsertBatch(state, CONV), null);
});

test("handoff migration moves source queue to the target without changing order", () => {
  const target = "https://chatgpt.com/c/target";
  let state = enqueue(normalizeQueuedInsertState({}), "source first", 0).state;
  state = enqueue(state, "source second", 1).state;
  const targetInsert = enqueueQueuedInsert(state, target, "target later", { now: BASE + 2, id: "target-1" });
  assert.equal(targetInsert.ok, true);
  const moved = moveQueuedInserts(targetInsert.state, CONV, target);
  assert.equal(moved.ok, true);
  assert.equal(moved.moved_count, 2);
  assert.equal(queuedInsertBatch(moved.state, CONV), null);
  assert.equal(queuedInsertBatch(moved.state, target).text, "source first\n\nsource second\n\ntarget later");
});
