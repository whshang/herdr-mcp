import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

const source = readFileSync(new URL("../extension/background.js", import.meta.url), "utf8");
const start = source.indexOf("function browserPageFillVerdict(");
const body = source.slice(start, source.indexOf("\n}\n", start) + 2);
const browserPageFillVerdict = new Function(`${body}; return browserPageFillVerdict;`)();

test("user fills a Claude composer that restored a draft | Given the before-text already equals the request | When the fill step did not prove a clear | Then fill is reported unverifiable instead of applied", () => {
  const want = "seed OK";
  const same = { ok: true, text: want };
  assert.deepEqual(
    browserPageFillVerdict({ before: same, after: same, want, result: { ok: true, committed: true } }),
    { applied: false, error: "fill_unverifiable_same_text", beforeMatched: true },
  );
  assert.equal(browserPageFillVerdict({ before: same, after: same, want, result: { ok: true, cleared: true } }).applied, true);
});

test("user fills a rich-text composer | Given before/after editor readbacks | When the after-text differs or the step failed | Then fill reports not applied", () => {
  const want = "hello";
  const empty = { ok: true, text: "" };
  assert.equal(browserPageFillVerdict({ before: empty, after: { ok: true, text: "hello" }, want, result: { ok: true } }).applied, true);
  assert.equal(browserPageFillVerdict({ before: empty, after: empty, want, result: { ok: true } }).error, "fill_not_applied");
  assert.equal(browserPageFillVerdict({ before: empty, after: empty, want, result: { ok: false, error: "fill_clear_not_applied" } }).error, "fill_clear_not_applied");
});
