import test from "node:test";
import assert from "node:assert/strict";
import { access, readFile, readdir, rm } from "node:fs/promises";
import { constants } from "node:fs";
import { spawnSync } from "node:child_process";
import { join } from "node:path";

const ROOT = new URL("../", import.meta.url).pathname;
const OUT = join(ROOT, "site-dist");

test("documentation site build publishes docs, release metadata and project skill without WIP", async () => {
  await rm(OUT, { recursive: true, force: true });
  const run = spawnSync(process.execPath, [join(ROOT, "scripts", "build-site.mjs")], {
    cwd: ROOT,
    encoding: "utf8",
    env: { ...process.env, HERDR_SITE_COMMIT: "site-build-test" },
  });
  assert.equal(run.status, 0, run.stderr || run.stdout);
  const result = JSON.parse(run.stdout.trim().split(/\r?\n/).at(-1));
  assert.equal(result.ok, true);
  assert.ok(result.docs >= 10);

  for (const rel of [
    "index.html",
    "style.css",
    "docs/index.html",
    "docs/automation.html",
    "docs/runtime-self-upgrade.html",
    "docs/worker-fallbacks.html",
    "herdr-mcp-SKILL.md",
    "release.json",
    ".nojekyll",
  ]) {
    await access(join(OUT, rel), constants.R_OK);
  }

  const release = JSON.parse(await readFile(join(OUT, "release.json"), "utf8"));
  const pkg = JSON.parse(await readFile(join(ROOT, "package.json"), "utf8"));
  assert.equal(release.version, pkg.version);
  assert.equal(release.commit, "site-build-test");
  assert.equal(release.docs, "./docs/");
  assert.equal(release.skill, "./herdr-mcp-SKILL.md");

  const generatedDocs = await readdir(join(OUT, "docs"));
  assert.equal(generatedDocs.some((name) => name.includes("_wip")), false);
  const skill = await readFile(join(OUT, "herdr-mcp-SKILL.md"), "utf8");
  assert.match(skill, /# herdr-mcp remote planner skill/);
  assert.match(skill, /dsh --profile headless/);
});
