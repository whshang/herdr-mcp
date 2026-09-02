import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

import { validateReleaseNotes } from "../scripts/validate-release-notes.mjs";

const ROOT = join(fileURLToPath(new URL("..", import.meta.url)));
const TAG = "v0.4.3";

test("v0.4.3 authored release notes satisfy the durable publication contract", async () => {
  const text = await readFile(join(ROOT, "docs/releases/v0.4.3.md"), "utf8");
  assert.deepEqual(validateReleaseNotes(TAG, text), []);
});

test("release notes reject Main changes bullets without implementation PR links", async () => {
  const text = await readFile(join(ROOT, "docs/releases/v0.4.3.md"), "utf8");
  const broken = text.replace(
    "https://github.com/whshang/herdr-mcp/pull/228",
    "PR-228",
  );
  const errors = validateReleaseNotes(TAG, broken);
  assert.ok(errors.some((error) => error.includes("each Main changes bullet")));
});

test("release notes require upgrade, known-issue and compatibility sections", async () => {
  const text = await readFile(join(ROOT, "docs/releases/v0.4.3.md"), "utf8");
  const broken = text
    .replace("## Upgrade from v0.4.2", "## Migration")
    .replace("## Known issues", "## Follow-up")
    .replace("## Compatibility", "## Support");
  const errors = validateReleaseNotes(TAG, broken);
  assert.ok(errors.some((error) => error.includes("Upgrade")));
  assert.ok(errors.some((error) => error.includes("Known issue")));
  assert.ok(errors.some((error) => error.includes("Compatibility")));
});
