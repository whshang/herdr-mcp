/**
 * commitAtomic rollback: newly added files must be removed on failure.
 */
import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, writeFile, readFile, access, mkdir } from "node:fs/promises";
import { tmpdir } from "node:os";
import * as path from "node:path";
import { commitAtomic } from "../dist/atomic-files.js";

test("commitAtomic rolls back newly created files when a later stage fails", async () => {
  const dir = await mkdtemp(path.join(tmpdir(), "herdr-atomic-"));
  const a = path.join(dir, "a.txt");
  const b = path.join(dir, "nested", "b.txt");
  await mkdir(path.join(dir, "nested"), { recursive: true });

  let threw = false;
  try {
    await commitAtomic([
      { real: a, content: "new-a\n" },
      { real: b, content: "new-b\n" },
      // Force failure: delete a non-existent file after the adds were applied in order.
      // commitAtomic applies in order — use a stage that unlinks a path we make fail by
      // pointing unlink at a directory (EISDIR / EISDIR-like) after creating a marker dir.
    ]);
  } catch {
    threw = true;
  }
  // First call with only adds should succeed
  assert.equal(threw, false);
  assert.equal(await readFile(a, "utf-8"), "new-a\n");

  // Second: add c, then fail on deleting a directory without recursive — use invalid delete target
  const c = path.join(dir, "c.txt");
  const blocker = path.join(dir, "blocker-dir");
  await mkdir(blocker);

  threw = false;
  try {
    await commitAtomic([
      { real: c, content: "new-c\n" },
      { real: blocker, content: null }, // unlink on directory should fail
    ]);
  } catch {
    threw = true;
  }
  assert.equal(threw, true, "expected commit to fail on directory unlink");
  await assert.rejects(() => access(c), /ENOENT/, "new file c.txt must be removed on rollback");
  // existing a.txt untouched
  assert.equal(await readFile(a, "utf-8"), "new-a\n");
});

test("commitAtomic restores backup when replace fails mid-flight", async () => {
  const dir = await mkdtemp(path.join(tmpdir(), "herdr-atomic2-"));
  const existing = path.join(dir, "keep.txt");
  await writeFile(existing, "original\n", "utf-8");

  let threw = false;
  try {
    await commitAtomic([
      { real: existing, content: "replaced\n" },
      // prepare writes tmp for ghost under missing parent — mkdir recursive should succeed;
      // force failure by deleting a missing file after replace
      { real: path.join(dir, "missing-delete.txt"), content: null },
    ]);
  } catch {
    threw = true;
  }
  assert.equal(threw, true);
  assert.equal(await readFile(existing, "utf-8"), "original\n");
});
