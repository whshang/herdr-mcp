import assert from "node:assert/strict";
import test from "node:test";
import {
  CONTINUITY_SEED_PREFIX,
  CONTINUITY_JOURNAL_STORAGE_KEY,
  buildContinuitySeed,
  continuityMessageId,
  continuitySeedContainsReference,
  stableHash,
  turnFingerprint,
} from "../extension/continuity-journal.js";

const CID = "hc:test:abc";
const CONV = "https://chatgpt.com/g/project/c/1";

test("journal exposes the shared storage key and a compact continuity seed", () => {
  assert.equal(typeof CONTINUITY_JOURNAL_STORAGE_KEY, "string");
  assert.ok(CONTINUITY_JOURNAL_STORAGE_KEY.length > 0);
  const seed = buildContinuitySeed({ transferId: "ht:1", continuityId: CID });
  assert.ok(seed.includes(CONTINUITY_SEED_PREFIX));
  assert.ok(seed.includes(`continuity_id=${CID}`));
  assert.ok(continuitySeedContainsReference(seed, "ht:1"));
  assert.equal(continuitySeedContainsReference(seed, "ht:other"), false);
});

test("continuity seed instructs continuity.resume then live revalidation", () => {
  const seed = buildContinuitySeed({ transferId: "ht:1", continuityId: CID });
  assert.ok(seed.includes('method 为 continuity.resume'));
  assert.ok(seed.includes("continuity.resume"));
  assert.ok(seed.includes("重新检查相关 Herdr/runtime/Git 实时状态"));
});

test("message id falls back to a deterministic fingerprint per side", () => {
  const startedAt = Date.now() - 1000;
  const user = continuityMessageId({ convKey: CONV, role: "user", text: "hello", startedAt });
  const assistant = continuityMessageId({ convKey: CONV, role: "assistant", text: "hi there", startedAt });
  assert.notEqual(user, assistant);
  const userAgain = continuityMessageId({ convKey: CONV, role: "user", text: "hello", startedAt });
  assert.equal(user, userAgain);
  // A page-provided message id is passed through verbatim.
  assert.equal(continuityMessageId({ messageId: "cm-123" }), "cm-123");
});

test("fingerprint is stable for identical text and distinct for changes", () => {
  const a = turnFingerprint({ convKey: CONV, startedAt: 10, userText: "x", assistantText: "y" });
  const b = turnFingerprint({ convKey: CONV, startedAt: 10, userText: "x", assistantText: "y" });
  const c = turnFingerprint({ convKey: CONV, startedAt: 10, userText: "x", assistantText: "z" });
  assert.equal(a, b);
  assert.notEqual(a, c);
});

test("stableHash is deterministic", () => {
  assert.equal(stableHash("a"), stableHash("a"));
  assert.notEqual(stableHash("a"), stableHash("b"));
});
