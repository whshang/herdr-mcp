import test from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, writeFile } from "node:fs/promises";
import { spawnSync } from "node:child_process";
import { tmpdir } from "node:os";
import { join } from "node:path";

const LINTER = new URL("../scripts/lint-findings.mjs", import.meta.url).pathname;
const REPO_ROOT = new URL("..", import.meta.url).pathname;

const FINDINGS_PREAMBLE = `# Findings

Lessons from daily commit reviews.

Template:

**<one-line rule>**
- <which commit / issue caused it, and the consequence>
- Seen N× (YYYY-MM-DD, YYYY-MM-DD).

---

`;

const RULES_PREAMBLE = `# Rules

Hard rules promoted from FINDINGS.md.

Entries use the template defined in [FINDINGS.md](FINDINGS.md).

---

`;

function entry(headline, seenLine) {
  return "**" + headline + "**\n- caused by #1, consequence noted\n" + seenLine + "\n\n";
}

async function fixture({ findings, rules = RULES_PREAMBLE + "_No rules promoted yet._\n" }) {
  const root = await mkdtemp(join(tmpdir(), "herdr-findings-lint-"));
  await writeFile(join(root, "FINDINGS.md"), findings);
  await writeFile(join(root, "RULES.md"), rules);
  return root;
}

function lint(root, extra = []) {
  return spawnSync(
    process.execPath,
    [LINTER, "--findings", join(root, "FINDINGS.md"), "--rules", join(root, "RULES.md"), ...extra],
    { cwd: root, encoding: "utf8" },
  );
}

test("user keeps the lesson logs green | Given the checked-in FINDINGS.md and RULES.md | When the promotion gate runs | Then it passes", () => {
  const result = spawnSync(process.execPath, [LINTER], { cwd: REPO_ROOT, encoding: "utf8" });
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /findings-lint: PASS/);
  assert.match(result.stdout, /FINDINGS\.md \d+ entries, RULES\.md \d+ entries/);
});

test("user writes a truthful count | Given an entry claiming 2× with 3 distinct review dates listed | When the gate runs | Then it names the mismatched entry", async () => {
  const root = await fixture({
    findings: FINDINGS_PREAMBLE + entry("Rule one", "- Seen 2× (2026-10-10, 2026-10-11, 2026-10-12)."),
  });
  const result = lint(root);
  assert.equal(result.status, 1);
  assert.match(result.stderr, /findings-lint: FAIL/);
  assert.match(result.stderr, /"Rule one" — `Seen 2×` but 3 distinct review date\(s\) are listed/);
});

test("user avoids repeating a review date | Given an entry listing the same date twice | When the gate runs | Then it reports the duplicate date", async () => {
  const root = await fixture({
    findings: FINDINGS_PREAMBLE + entry("Rule one", "- Seen 2× (2026-10-12, 2026-10-12)."),
  });
  const result = lint(root);
  assert.equal(result.status, 1);
  assert.match(result.stderr, /duplicate review date\(s\) listed more than once: 2026-10-12/);
});

test("user rejects an impossible review date | Given an entry with a malformed date | When the gate runs | Then it reports the invalid date", async () => {
  const root = await fixture({
    findings: FINDINGS_PREAMBLE + entry("Rule one", "- Seen 1× (2026-13-01)."),
  });
  const result = lint(root);
  assert.equal(result.status, 1);
  assert.match(result.stderr, /invalid review date\(s\) 2026-13-01/);
});

test("user promotes a matured lesson | Given an entry still in FINDINGS.md with 3 distinct review dates | When the gate runs | Then it fails and names the entry", async () => {
  const root = await fixture({
    findings: FINDINGS_PREAMBLE + entry("Rule one", "- Seen 3× (2026-10-10, 2026-10-11, 2026-10-12)."),
  });
  const result = lint(root);
  assert.equal(result.status, 1);
  assert.match(result.stderr, /"Rule one" — 3 distinct review dates; promote it to RULES\.md in this change/);
});

test("user moves instead of copying a promoted lesson | Given the same headline in FINDINGS.md and RULES.md | When the gate runs | Then it reports the headline living in both files", async () => {
  const root = await fixture({
    findings: FINDINGS_PREAMBLE + entry("Rule one", "- Seen 2× (2026-10-10, 2026-10-11)."),
    rules: RULES_PREAMBLE + entry("Rule one", "- Seen 3× (2026-10-10, 2026-10-11, 2026-10-12)."),
  });
  const result = lint(root);
  assert.equal(result.status, 1);
  assert.match(result.stderr, /"Rule one" — headline appears in both FINDINGS\.md and RULES\.md/);
});

test("user edits the existing entry | Given the same headline twice in FINDINGS.md | When the gate runs | Then it reports the duplicate headline", async () => {
  const root = await fixture({
    findings:
      FINDINGS_PREAMBLE +
      entry("Rule one", "- Seen 1× (2026-10-10).") +
      entry("Rule one", "- Seen 2× (2026-10-10, 2026-10-11)."),
  });
  const result = lint(root);
  assert.equal(result.status, 1);
  assert.match(result.stderr, /"Rule one" — headline appears 2× in FINDINGS\.md/);
});

test("user keeps the template fields | Given an entry without a Seen line | When the gate runs | Then it reports the missing line", async () => {
  const root = await fixture({
    findings: FINDINGS_PREAMBLE + "**Rule one**\n- caused by #1, consequence noted\n",
  });
  const result = lint(root);
  assert.equal(result.status, 1);
  assert.match(result.stderr, /"Rule one" — missing `- Seen N× \(YYYY-MM-DD, \.\.\.\)\.` line/);
});

test("user sees what is due for promotion | Given a 2-date entry and a 3-date entry | When candidates mode runs | Then it lists both and does not fail", async () => {
  const root = await fixture({
    findings:
      FINDINGS_PREAMBLE +
      entry("Upcoming lesson", "- Seen 2× (2026-10-10, 2026-10-11).") +
      entry("Matured lesson", "- Seen 3× (2026-10-10, 2026-10-11, 2026-10-12)."),
  });
  const result = lint(root, ["--candidates"]);
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /upcoming \(2 distinct review dates; promote on the next hit\):\n {4}- 2× Upcoming lesson/);
  assert.match(result.stdout, /required now \(3\+ distinct review dates;[^\n]*\):\n {4}- 3× Matured lesson/);
  assert.match(result.stdout, /1 entry must be promoted \(candidates mode does not fail\)/);
});

test("user cannot fork a lesson with a trailing period | Given the headlines **Rule one** and **Rule one.** in FINDINGS.md | When the gate runs | Then it treats both as the same lesson and fails", async () => {
  const root = await fixture({
    findings:
      FINDINGS_PREAMBLE +
      entry("Rule one", "- Seen 1× (2026-10-10).") +
      entry("Rule one.", "- Seen 1× (2026-10-11)."),
  });
  const result = lint(root);
  assert.equal(result.status, 1);
  assert.match(result.stderr, /"Rule one" — headline appears 2× in FINDINGS\.md/);
});

test("user cannot punctuate a promoted lesson to keep it alive | Given FINDINGS.md **Rule one.** and RULES.md **Rule one** | When the gate runs | Then it reports the same headline in both files", async () => {
  const root = await fixture({
    findings: FINDINGS_PREAMBLE + entry("Rule one.", "- Seen 2× (2026-10-10, 2026-10-11)."),
    rules: RULES_PREAMBLE + entry("Rule one", "- Seen 3× (2026-10-10, 2026-10-11, 2026-10-12)."),
  });
  const result = lint(root);
  assert.equal(result.status, 1);
  assert.match(result.stderr, /"Rule one\." — headline appears in both FINDINGS\.md and RULES\.md/);
});

test("user keeps formatting meaningful | Given two headlines that differ only by code formatting | When the gate runs | Then they stay distinct lessons and it passes", async () => {
  const root = await fixture({
    findings:
      FINDINGS_PREAMBLE +
      entry("Keep `runtime/current` symlinked", "- Seen 1× (2026-10-10).") +
      entry("Keep runtime/current symlinked", "- Seen 1× (2026-10-11)."),
  });
  const result = lint(root);
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /findings-lint: PASS/);
});
