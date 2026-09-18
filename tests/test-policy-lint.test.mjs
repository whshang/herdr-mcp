import test from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, mkdir, writeFile } from "node:fs/promises";
import { execFileSync, spawnSync } from "node:child_process";
import { tmpdir } from "node:os";
import { join } from "node:path";

const LINTER = new URL("../scripts/lint-test-policy.mjs", import.meta.url).pathname;

function run(cwd, program, args) {
  return execFileSync(program, args, { cwd, encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] });
}

async function repoWithBase() {
  const root = await mkdtemp(join(tmpdir(), "herdr-test-policy-"));
  run(root, "git", ["init", "-q"]);
  run(root, "git", ["config", "user.email", "ci@example.invalid"]);
  run(root, "git", ["config", "user.name", "CI"]);
  await mkdir(join(root, "tests"), { recursive: true });
  await writeFile(join(root, "README.md"), "base\n");
  run(root, "git", ["add", "."]);
  run(root, "git", ["commit", "-qm", "base"]);
  return { root, base: run(root, "git", ["rev-parse", "HEAD"]).trim() };
}

test("user adds an acceptance test | Given a user-prefixed Given When Then title | When test policy lint runs | Then the change passes", async () => {
  const { root, base } = await repoWithBase();
  await writeFile(
    join(root, "tests", "acceptance.test.mjs"),
    'import test from "node:test";\ntest("user retries an order | Given one completed order | When the same request repeats | Then only one charge is visible", () => {});\n',
  );
  run(root, "git", ["add", "."]);
  run(root, "git", ["commit", "-qm", "acceptance"]);
  const result = spawnSync(process.execPath, [LINTER, base], { cwd: root, encoding: "utf8" });
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /test-policy: PASS/);
});

test("user adds implementation-coupled test machinery | Given a non-BDD title and mock API | When test policy lint runs | Then CI rejects both policy violations", async () => {
  const { root, base } = await repoWithBase();
  const forbiddenMockCall = ["t", "mock", "method"].join(".");
  await writeFile(
    join(root, "tests", "implementation.test.mjs"),
    'import test from "node:test";\ntest("calls helper twice", (t) => { ' + forbiddenMockCall + '({}, "run", () => {}); });\n',
  );
  run(root, "git", ["add", "."]);
  run(root, "git", ["commit", "-qm", "implementation test"]);
  const result = spawnSync(process.execPath, [LINTER, base], { cwd: root, encoding: "utf8" });
  assert.equal(result.status, 1);
  assert.match(result.stderr, /new test title must start with "user "/);
  assert.match(result.stderr, /mocking APIs are not allowed/);
});
