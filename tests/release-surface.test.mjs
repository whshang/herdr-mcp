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
  "herdr-extension-host": "bin/herdr-extension-host",
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
  assert.match(edge, /!edge\/cloudflare\/\*\*\/\*\.md/, "Edge docs-only changes must not redeploy the production Worker");
  assert.doesNotMatch(edge, /cloudflared|DNS Write|Tunnel/);
});

test("Rust release verification provisions and always cleans the pinned Herdr runtime", async () => {
  const release = await readFile(join(ROOT, ".github/workflows/rust-release.yml"), "utf8");
  const start = release.indexOf("scripts/ci-herdr-runtime.sh start");
  const rootTests = release.indexOf("npm test");
  const stop = release.indexOf("scripts/ci-herdr-runtime.sh stop");
  assert.ok(start >= 0, "release verify must start the pinned Herdr runtime");
  assert.ok(rootTests > start, "runtime must be ready before root transport tests");
  assert.ok(stop > rootTests, "runtime cleanup must run after root transport tests");
  assert.match(
    release,
    /- name: Stop Herdr runtime\n\s+if: always\(\)\n\s+run: scripts\/ci-herdr-runtime\.sh stop/,
    "release verify must clean the pinned Herdr runtime even after a failed gate",
  );
});

test("Rust release defaults to macOS ARM64 and Windows x64 artifacts only", async () => {
  const release = await readFile(join(ROOT, ".github/workflows/rust-release.yml"), "utf8");
  const buildJob = release.slice(release.indexOf("\n  build:\n"), release.indexOf("\n  manifest:\n"));
  assert.match(buildJob, /runner: macos-15\n\s+target: aarch64-apple-darwin/);
  assert.match(buildJob, /runner: windows-2025\n\s+target: x86_64-pc-windows-msvc/);
  assert.doesNotMatch(buildJob, /x86_64-apple-darwin/);
  assert.doesNotMatch(buildJob, /unknown-linux-gnu/);
});

test("Rust GitHub Release provenance is tag-only and fail-closed before publish", async () => {
  const release = await readFile(join(ROOT, ".github/workflows/rust-release.yml"), "utf8");
  const attestJob = release.indexOf("\n  attest:\n");
  const publishJob = release.indexOf("\n  publish:\n");
  assert.ok(attestJob >= 0, "release workflow must have a dedicated attestation job");
  assert.ok(publishJob > attestJob, "attestation must be declared before GitHub Release publishing");
  const attest = release.slice(attestJob, publishJob);
  const publish = release.slice(publishJob);
  assert.match(attest, /needs: manifest/);
  assert.match(attest, /if: startsWith\(github\.ref, 'refs\/tags\/v'\)/);
  assert.match(attest, /contents: read/);
  assert.match(attest, /id-token: write/);
  assert.match(attest, /attestations: write/);
  assert.match(attest, /artifact-metadata: write/);
  assert.match(
    attest,
    /uses: actions\/attest@1e69f48acb82d1966a394da916b4c1698aa569d6 # v4\.2\.2/,
    "release provenance action must be pinned to the reviewed v4.2.2 commit",
  );
  assert.match(attest, /release-assets\/herdr-mcp-\*/);
  assert.match(attest, /release-assets\/release-manifest\.json/);
  assert.match(release, /--repository-id \"\$GITHUB_REPOSITORY_ID\"/);
  assert.match(release, /--source-commit \"\$GITHUB_SHA\"/);
  assert.match(release, /--source-ref \"\$GITHUB_REF\"/);
  assert.match(release, /--workflow-name \"\$GITHUB_WORKFLOW\"/);
  assert.match(publish, /needs: attest/, "GitHub Release publishing must fail closed when attestation fails");
  assert.match(publish, /if: startsWith\(github\.ref, 'refs\/tags\/v'\)/);
  assert.match(
    publish,
    /GH_REPO: \$\{\{ github\.repository \}\}/,
    "publish must name the GitHub repository explicitly because the job does not checkout git metadata",
  );
  assert.match(publish, /release_flags=\(--verify-tag --generate-notes\)/);
  assert.match(publish, /release_flags\+=\(--prerelease\)/, "semver prereleases must be marked as GitHub prereleases");
  assert.doesNotMatch(attest, /workflow_dispatch/);
  assert.doesNotMatch(publish, /workflow_dispatch/);
});

test("Rust Release recovery republishes only a previously attested GitHub run", async () => {
  const recovery = await readFile(join(ROOT, ".github/workflows/rust-release-recover.yml"), "utf8");
  assert.match(recovery, /workflow_dispatch:/);
  assert.match(recovery, /source_run_id:/);
  assert.match(recovery, /tag:/);
  assert.match(recovery, /actions: read/);
  assert.match(recovery, /contents: write/);
  assert.match(recovery, /attestations: read/);
  assert.match(recovery, /run-id: \$\{\{ inputs\.source_run_id \}\}/);
  assert.match(recovery, /name: rust-release-bundle/);
  assert.match(recovery, /\.github\/workflows\/rust-release\.yml/);
  assert.match(recovery, /head_sha/);
  assert.match(recovery, /conclusion==\"success\"/);
  assert.match(recovery, /gh attestation verify/);
  assert.match(recovery, /--signer-workflow/);
  assert.match(recovery, /--source-ref/);
  assert.match(recovery, /--source-digest/);
  assert.match(recovery, /--deny-self-hosted-runners/);
  assert.match(recovery, /gh release create/);
  assert.match(recovery, /--verify-tag/);
  assert.match(recovery, /--prerelease/);
});

test("local Cloudflare/Pages build state is excluded from the published package", async () => {
  const rootIgnore = await readFile(join(ROOT, ".gitignore"), "utf8");
  const edgeIgnore = await readFile(join(ROOT, "edge/cloudflare/.gitignore"), "utf8");
  assert.match(rootIgnore, /edge\/cloudflare\/\.wrangler\//);
  assert.match(rootIgnore, /site-dist\//);
  assert.match(rootIgnore, /docs\/_wip\//);
  assert.match(edgeIgnore, /^\.wrangler\/$/m);
});
