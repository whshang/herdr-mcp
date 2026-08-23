import test from "node:test";
import assert from "node:assert/strict";
import { access, readFile, stat } from "node:fs/promises";
import { constants } from "node:fs";
import { join } from "node:path";

const ROOT = new URL("../", import.meta.url).pathname;
const pkg = JSON.parse(await readFile(join(ROOT, "package.json"), "utf8"));
const lock = JSON.parse(await readFile(join(ROOT, "package-lock.json"), "utf8"));

const EXPECTED_BINS = {
  "herdr-mcp": "dist/server.js",
  "herdr-cloudflare-token": "bin/herdr-cloudflare-token",
  "herdr-cloudflare-dns-token": "bin/herdr-cloudflare-dns-token",
  "herdr-cloudflare-domain": "bin/herdr-cloudflare-domain",
  "herdr-custom-domain-cutover": "bin/herdr-custom-domain-cutover",
  "herdr-runtime-generation": "bin/herdr-runtime-generation",
  "herdr-self-update": "bin/herdr-self-update",
};

test("package and lock expose the complete public CLI surface", () => {
  assert.deepEqual(pkg.bin, EXPECTED_BINS);
  assert.deepEqual(lock.packages?.[""]?.bin, EXPECTED_BINS);
  assert.equal(pkg.version, lock.packages?.[""]?.version);
});

test("non-node public CLI files are executable", async () => {
  for (const [name, rel] of Object.entries(EXPECTED_BINS)) {
    if (rel === "dist/server.js") continue;
    const path = join(ROOT, rel);
    await access(path, constants.X_OK);
    const mode = (await stat(path)).mode;
    assert.notEqual(mode & 0o111, 0, `${name} must be executable`);
  }
});

test("CI/CD and documentation publishing entrypoints are tracked in the release tree", async () => {
  for (const rel of [
    ".github/workflows/ci.yml",
    ".github/workflows/pages.yml",
    ".github/workflows/cloudflare-edge.yml",
    "scripts/build-site.mjs",
    "assets/herdr-mcp-SKILL.md",
    "site/index.html",
  ]) {
    await access(join(ROOT, rel), constants.R_OK);
  }
  assert.equal(pkg.scripts?.["build:site"], "node scripts/build-site.mjs");
  assert.equal(pkg.scripts?.["self:update"], "bin/herdr-self-update");
});

test("Actions build docs and keep Cloudflare deployment on the gated Worker plane", async () => {
  const ci = await readFile(join(ROOT, ".github/workflows/ci.yml"), "utf8");
  const pages = await readFile(join(ROOT, ".github/workflows/pages.yml"), "utf8");
  const edge = await readFile(join(ROOT, ".github/workflows/cloudflare-edge.yml"), "utf8");
  assert.match(ci, /npm run build:site/);
  assert.match(ci, /extension_smoke\.mjs/);
  assert.match(pages, /npm run build:site/);
  assert.match(pages, /path: site-dist/);
  assert.match(pages, /actions\/deploy-pages@v4/);
  assert.match(edge, /cloudflare\/wrangler-action@v4/);
  assert.match(edge, /environment: production/);
  assert.match(edge, /wranglerVersion: "4"/);
  assert.match(edge, /wrangler\.prod\.toml/);
  assert.doesNotMatch(edge, /cloudflared|DNS Write|Tunnel/);
});

test("local Cloudflare/Pages build state is excluded from the published package", async () => {
  const rootIgnore = await readFile(join(ROOT, ".gitignore"), "utf8");
  const edgeIgnore = await readFile(join(ROOT, "edge/cloudflare/.gitignore"), "utf8");
  assert.match(rootIgnore, /edge\/cloudflare\/\.wrangler\//);
  assert.match(rootIgnore, /site-dist\//);
  assert.match(rootIgnore, /docs\/_wip\//);
  assert.match(edgeIgnore, /^\.wrangler\/$/m);
});
