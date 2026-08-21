/**
 * Unit tests for apply-patch envelope (no live herdr).
 */
import { test } from "node:test";
import assert from "node:assert/strict";
import { parsePatch, applyUpdateHunks, PatchError } from "../dist/patch.js";

test("parse add/update/delete envelope", () => {
  const ops = parsePatch(`*** Begin Patch
*** Add File: a.txt
+hello
*** Update File: b.txt
@@
-old
+new
*** Delete File: c.txt
*** End Patch`);
  assert.equal(ops.length, 3);
  assert.equal(ops[0].kind, "add");
  assert.equal(ops[1].kind, "update");
  assert.equal(ops[2].kind, "delete");
});

test("applyUpdateHunks unique context", () => {
  const out = applyUpdateHunks("one\nold\ntwo\n", [[" one", "-old", "+new", " two"]], "b.txt");
  assert.equal(out, "one\nnew\ntwo\n");
});

test("ambiguous context throws", () => {
  assert.throws(
    () => applyUpdateHunks("x\nx\n", [["-x", "+y"]], "f"),
    (e) => e instanceof PatchError && e.code === "PATCH_CONTEXT_AMBIGUOUS",
  );
});
