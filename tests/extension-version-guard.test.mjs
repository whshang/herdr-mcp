import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtempSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { after, test } from "node:test";
import { fileURLToPath } from "node:url";

const SCRIPT = join(
  dirname(fileURLToPath(import.meta.url)),
  "..",
  "scripts",
  "check-extension-version.mjs",
);
const roots = [];

after(() => {
  for (const root of roots) rmSync(root, { recursive: true, force: true });
});

function git(cwd, argv) {
  return execFileSync(
    "git",
    ["-c", "commit.gpgsign=false", "-c", "tag.gpgsign=false", ...argv],
    { cwd, encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] },
  ).trim();
}

function repository() {
  const root = mkdtempSync(join(tmpdir(), "herdr-extension-version-guard-"));
  roots.push(root);
  git(root, ["init", "-q", "-b", "main"]);
  git(root, ["config", "user.email", "guard@example.test"]);
  git(root, ["config", "user.name", "Extension Guard"]);
  return root;
}

/** Shallow clones must come from a parent directory: the origin dir nests the clone path. */
function cloneParent(origin) {
  return dirname(origin);
}

function writeExtension(root, { version, marker = "bundle-1", fileName = "background.js" }) {
  mkdirSync(join(root, "extension"), { recursive: true });
  writeFileSync(
    join(root, "extension", "manifest.json"),
    `${JSON.stringify({ manifest_version: 3, name: "Herdr", version }, null, 2)}\n`,
  );
  writeFileSync(join(root, "extension", fileName), `// ${marker}\n`);
}

function commitAll(root, message) {
  git(root, ["add", "-A"]);
  git(root, ["commit", "-q", "-m", message]);
}

/** A tag-less shallow clone of `origin`, with the local identity a fresh clone lacks. */
function shallowClone(origin) {
  const clone = `${origin}-shallow`;
  git(cloneParent(origin), [
    "clone",
    "-q",
    "--depth=1",
    "--no-tags",
    `file://${origin}`,
    clone,
  ]);
  roots.push(clone);
  git(clone, ["config", "user.email", "guard@example.test"]);
  git(clone, ["config", "user.name", "Extension Guard"]);
  return clone;
}

function releaseTag(root, version, tag = "v1.0.0") {
  writeExtension(root, { version, marker: "released-bundle" });
  commitAll(root, `release ${version}`);
  git(root, ["tag", tag]);
}

function runGuard(root, extraArgs = ["--baseline", "v1.0.0"]) {
  try {
    const stdout = execFileSync(process.execPath, [SCRIPT, ...extraArgs], {
      cwd: root,
      encoding: "utf8",
      stdio: ["ignore", "pipe", "pipe"],
    });
    return { status: 0, stdout };
  } catch (error) {
    return {
      status: error.status ?? 1,
      stdout: `${error.stdout ?? ""}${error.stderr ?? ""}`,
    };
  }
}

test(
  "user gets a passing guard | Given extension files changed since the release tag | When the manifest version was bumped | Then the guard passes and names both versions",
  () => {
    const root = repository();
    releaseTag(root, "0.1.229");
    writeExtension(root, { version: "0.1.230", marker: "bundle-2" });
    commitAll(root, "fix(extension): ship the next build");

    const result = runGuard(root);

    assert.equal(result.status, 0, result.stdout);
    assert.match(result.stdout, /PASS/);
    assert.match(result.stdout, /0\.1\.229 -> 0\.1\.230/);
  },
);

test(
  "user is blocked before shipping an unlabelled extension build | Given extension files changed since the release tag | When the manifest version was not bumped | Then the guard fails and lists the changed files",
  () => {
    const root = repository();
    releaseTag(root, "0.1.229");
    writeExtension(root, { version: "0.1.229", marker: "bundle-2" });
    commitAll(root, "fix(extension): ship the next build without a bump");

    const result = runGuard(root);

    assert.equal(result.status, 1, result.stdout);
    assert.match(result.stdout, /FAIL/);
    assert.match(result.stdout, /extension\/background\.js/);
    assert.match(result.stdout, /H2W_SCRIPT_VERSION/);
  },
);

test(
  "user is not asked to bump an untouched extension | Given extension/ is identical to the release tag | When the guard runs | Then it passes without requiring a new version",
  () => {
    const root = repository();
    releaseTag(root, "0.1.229");
    writeFileSync(join(root, "README.md"), "docs only\n");
    commitAll(root, "docs: unrelated change");

    const result = runGuard(root);

    assert.equal(result.status, 0, result.stdout);
    assert.match(result.stdout, /unchanged since v1\.0\.0/);
  },
);

test(
  "user cannot lower the released extension version | Given extension files changed since the release tag | When the manifest version went backwards | Then the guard fails",
  () => {
    const root = repository();
    releaseTag(root, "0.1.229");
    writeExtension(root, { version: "0.1.228", marker: "bundle-0" });
    commitAll(root, "chore(extension): wrong version");

    const result = runGuard(root);

    assert.equal(result.status, 1, result.stdout);
    assert.match(result.stdout, /FAIL/);
  },
);

test(
  "user gets a skip instead of a false failure | Given a repository with no release tag | When the guard runs | Then it skips and says why",
  () => {
    const root = repository();
    writeExtension(root, { version: "0.1.229", marker: "unreleased" });
    commitAll(root, "feat(extension): first commit");

    const result = runGuard(root, []);

    assert.equal(result.status, 0, result.stdout);
    assert.match(result.stdout, /SKIP/);
  },
);

test(
  "user gets a verified baseline in a shallow CI checkout | Given the release tag exists only on origin and the checkout is shallow with no tags | When the guard runs | Then it fetches the tag into refs/tags and compares against the released version",
  () => {
    const origin = repository();
    releaseTag(origin, "0.1.229");
    const clone = `${origin}-shallow`;
    git(cloneParent(origin), [
      "clone",
      "-q",
      "--depth=1",
      "--no-tags",
      `file://${origin}`,
      clone,
    ]);
    roots.push(clone);
    // A fresh clone does not inherit origin's repo-local git identity, and CI
    // runners have none globally, so set it before committing in the clone.
    git(clone, ["config", "user.email", "guard@example.test"]);
    git(clone, ["config", "user.name", "Extension Guard"]);
    writeExtension(clone, { version: "0.1.230", marker: "bundle-2" });
    commitAll(clone, "fix(extension): ship the next build");
    assert.equal(git(clone, ["tag", "--list", "v*"]), "");

    const result = runGuard(clone, []);

    assert.equal(result.status, 0, result.stdout);
    assert.match(result.stdout, /PASS/);
    assert.match(result.stdout, /0\.1\.229 -> 0\.1\.230/);
  },
);

test(
  "user gets the newest published release as baseline | Given origin also carries archive/v1.0-* evidence tags beside two release candidates | When the guard runs in a shallow checkout with no local tags | Then it names v1.1.0-rc.14 and never the archive tag",
  () => {
    const origin = repository();
    releaseTag(origin, "0.1.90", "archive/v1.0-evidence-semantics-20260905");
    releaseTag(origin, "0.1.200", "v1.1.0-rc.9");
    releaseTag(origin, "0.1.243", "v1.1.0-rc.14");
    const clone = shallowClone(origin);
    writeFileSync(join(clone, "README.md"), "docs only\n");
    commitAll(clone, "docs: unrelated change");
    assert.equal(git(clone, ["tag", "--list", "v*"]), "");

    const result = runGuard(clone, []);

    assert.equal(result.status, 0, result.stdout);
    assert.match(result.stdout, /unchanged since v1\.1\.0-rc\.14 \(0\.1\.243\)/);
    assert.doesNotMatch(result.stdout, /archive/);
  },
);

test(
  "user gets the numerically newest release candidate | Given origin has v1.1.0-rc.9 and v1.1.0-rc.14 | When the guard runs in a shallow checkout with no local tags | Then it names rc.14 instead of the lexicographically earlier rc.9",
  () => {
    const origin = repository();
    releaseTag(origin, "0.1.200", "v1.1.0-rc.9");
    releaseTag(origin, "0.1.243", "v1.1.0-rc.14");
    const clone = shallowClone(origin);
    writeFileSync(join(clone, "README.md"), "docs only\n");
    commitAll(clone, "docs: unrelated change");
    assert.equal(git(clone, ["tag", "--list", "v*"]), "");

    const result = runGuard(clone, []);

    assert.equal(result.status, 0, result.stdout);
    assert.match(result.stdout, /unchanged since v1\.1\.0-rc\.14 \(0\.1\.243\)/);
  },
);
