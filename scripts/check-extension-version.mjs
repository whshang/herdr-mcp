#!/usr/bin/env node
/**
 * Browser-extension version guard (#833).
 *
 * `extension/manifest.json` version is the only thing the Runtime can compare
 * against: the browser extension reports it on endpoint registration and
 * `doctor` uses it to decide "reload pending" vs "extension stale". When two
 * extension builds share one manifest version (0.1.229 was shipped across many
 * builds), neither the Runtime nor the user can tell that the running extension
 * is old — which is exactly how #833 hid on PROD.
 *
 * The guard compares the extension tree of HEAD with the extension tree of the
 * most recent `v*` release tag:
 *   - extension tree unchanged  -> pass (no version bump required);
 *   - extension tree changed    -> `extension/manifest.json` version MUST be
 *     greater than the tag's version.
 *
 * Comparing against the release tag (not against the change base) is what makes
 * this catch a missed bump that was merged in an earlier PR, and it keeps stale
 * branches from producing false failures: a branch whose extension tree still
 * equals the released tree passes without a bump.
 *
 * Usage:
 *   node scripts/check-extension-version.mjs [--baseline <git-ref>] [<git-ref>]
 *
 * Flags:
 *   --baseline <git-ref>  compare against this ref instead of the newest v* tag
 *   --strict              treat "no resolvable release tag" as a failure (used by
 *                         release packaging, where an unverifiable version must
 *                         not ship)
 *
 * Env:
 *   EXTENSION_VERSION_BASELINE_TAG  explicit baseline ref (default: newest v* tag)
 *
 * Exit codes: 0 pass or skip (no release tag can be resolved), 1 violation.
 */
import { execFileSync } from "node:child_process";

const args = process.argv.slice(2);
const strict = args.includes("--strict") || process.env.EXTENSION_VERSION_GUARD_STRICT === "1";
let baselineArg = process.env.EXTENSION_VERSION_BASELINE_TAG?.trim() || "";
for (let index = 0; index < args.length; index += 1) {
  const arg = args[index];
  if (arg === "--baseline") {
    baselineArg = String(args[index + 1] || "").trim();
    index += 1;
  } else if (!arg.startsWith("-")) {
    baselineArg = arg;
  }
}

function git(argv, { allowFailure = false } = {}) {
  try {
    return execFileSync("git", argv, {
      encoding: "utf8",
      stdio: ["ignore", "pipe", "pipe"],
    }).trim();
  } catch (error) {
    if (allowFailure) return null;
    throw error;
  }
}

function resolves(ref) {
  return git(["rev-parse", "--verify", `${ref}^{commit}`], { allowFailure: true }) !== null;
}

/**
 * A tag the guard may compare against: `v<dotted numeric core>` with an optional
 * pre-release. Evidence tags such as `archive/v1.0-evidence-semantics-20260905`
 * share a `v*` tail, so shape — not just the `v` prefix — decides the baseline.
 */
const RELEASE_TAG = /^v\d+(?:\.\d+)*(?:-[0-9A-Za-z.-]+)?$/;

function isReleaseVersionTag(tag) {
  return RELEASE_TAG.test(String(tag).trim());
}

/** Compare dot-separated pre-release identifiers: numeric by value, then lexical. */
function comparePrereleaseIdentifiers(left, right) {
  const a = left ? left.split(".") : [];
  const b = right ? right.split(".") : [];
  if (left === right) return 0;
  if (a.length === 0) return 1;
  if (b.length === 0) return -1;
  const length = Math.max(a.length, b.length);
  for (let index = 0; index < length; index += 1) {
    const leftPart = a[index];
    const rightPart = b[index];
    if (leftPart === undefined) return -1;
    if (rightPart === undefined) return 1;
    if (leftPart === rightPart) continue;
    const leftNumeric = /^\d+$/.test(leftPart);
    const rightNumeric = /^\d+$/.test(rightPart);
    if (leftNumeric && rightNumeric) {
      return Number(leftPart) < Number(rightPart) ? -1 : 1;
    }
    if (leftNumeric !== rightNumeric) return leftNumeric ? -1 : 1;
    return leftPart < rightPart ? -1 : 1;
  }
  return 0;
}

/** Order dotted numeric versions; pre-release suffixes sort before the release. */
export function compareExtensionVersions(left, right) {
  const parse = (value) => {
    const [core, ...rest] = String(value).trim().split("-");
    const numbers = core.split(".").map((part) => Number.parseInt(part, 10));
    if (numbers.some((part) => !Number.isFinite(part))) {
      throw new Error(`extension manifest version is not dotted numeric: ${value}`);
    }
    return { numbers, prerelease: rest.join("-") };
  };
  const a = parse(left);
  const b = parse(right);
  const length = Math.max(a.numbers.length, b.numbers.length);
  for (let index = 0; index < length; index += 1) {
    const leftPart = a.numbers[index] ?? 0;
    const rightPart = b.numbers[index] ?? 0;
    if (leftPart !== rightPart) return leftPart < rightPart ? -1 : 1;
  }
  return comparePrereleaseIdentifiers(a.prerelease, b.prerelease);
}

function newestReleaseTags() {
  const local = git(["tag", "--list", "v*", "--sort=-v:refname"], { allowFailure: true });
  const tags = local ? local.split("\n").filter(isReleaseVersionTag) : [];
  if (tags.some(resolves)) return tags;
  // A shallow CI checkout has no tags; fetch just the tag refs so the guard can
  // still compare against the last real release instead of silently skipping.
  const remote = git(["ls-remote", "--tags", "--refs", "origin", "v*"], {
    allowFailure: true,
  });
  if (!remote) return tags;
  // `ls-remote` matches `v*` against the last path segment, so it also returns
  // `refs/tags/archive/v1.0-*` evidence tags. Only release-shaped names can be
  // a baseline: an archive tag's `archive/v1.0` core is not dotted numeric, so
  // comparing it raised and the old fail-open sort kept ls-remote order.
  const remoteTags = remote
    .split("\n")
    .filter(Boolean)
    .map((line) => line.split("\t")[1]?.replace("refs/tags/", "") || "")
    .filter(isReleaseVersionTag);
  // After the shape filter every name parses, so ordering never fails open.
  remoteTags.sort((left, right) =>
    -compareExtensionVersions(left.replace(/^v/, ""), right.replace(/^v/, "")),
  );
  for (const tag of remoteTags) {
    if (resolves(tag)) return [tag, ...tags];
    // The destination refspec is required: a source-only refspec leaves the
    // fetched tag in FETCH_HEAD and never creates refs/tags/<tag>, so
    // `resolves` would keep failing and --strict CI runs would skip the guard.
    git(
      ["fetch", "--no-tags", "--depth=1", "origin", `+refs/tags/${tag}:refs/tags/${tag}`],
      { allowFailure: true },
    );
    if (resolves(tag)) return [tag, ...tags];
  }
  return tags;
}

function resolveBaseline() {
  if (baselineArg) {
    if (!resolves(baselineArg)) {
      throw new Error(`baseline ref ${baselineArg} does not resolve`);
    }
    return baselineArg;
  }
  const candidate = newestReleaseTags().find(resolves);
  return candidate || null;
}

function manifestAt(ref) {
  const raw = git(["show", `${ref}:extension/manifest.json`], { allowFailure: true });
  if (!raw) return null;
  try {
    return JSON.parse(raw);
  } catch {
    return null;
  }
}

function skip(message) {
  console.log(`extension-version-guard: ${strict ? "FAIL" : "SKIP"} · ${message}`);
  return strict ? 1 : 0;
}

function main() {
  const baseline = resolveBaseline();
  if (!baseline) {
    return skip(
      "no v* release tag resolves locally or on origin, so there is no released extension version to compare against",
    );
  }
  const baselineManifest = manifestAt(baseline);
  if (!baselineManifest?.version) {
    return skip(`baseline ${baseline} has no extension/manifest.json version`);
  }
  const changed = git(["diff", "--name-only", baseline, "HEAD", "--", "extension/"], {
    allowFailure: true,
  });
  const changedFiles = changed ? changed.split("\n").filter(Boolean) : [];
  if (changedFiles.length === 0) {
    console.log(
      `extension-version-guard: PASS · extension/ is unchanged since ${baseline} (${baselineManifest.version})`,
    );
    return 0;
  }
  const headManifest = manifestAt("HEAD");
  if (!headManifest?.version) {
    console.log(
      "extension-version-guard: FAIL · extension/ changed since " +
        `${baseline} but extension/manifest.json has no readable version`,
    );
    return 1;
  }
  const order = compareExtensionVersions(headManifest.version, baselineManifest.version);
  if (order > 0) {
    console.log(
      `extension-version-guard: PASS · extension/ changed since ${baseline} and extension/manifest.json was bumped ${baselineManifest.version} -> ${headManifest.version}`,
    );
    return 0;
  }
  console.log(
    `extension-version-guard: FAIL · ${changedFiles.length} extension/ file(s) changed since ${baseline} ` +
      `but extension/manifest.json version is still ${headManifest.version} (released: ${baselineManifest.version}). ` +
      "Bump extension/manifest.json (and H2W_SCRIPT_VERSION in extension/background.js plus H2W_CONTENT_VERSION in extension/content/wake.js) " +
      "so the Runtime and doctor can tell these builds apart.",
  );
  for (const file of changedFiles) console.log(`  changed: ${file}`);
  return 1;
}

process.exitCode = main();
