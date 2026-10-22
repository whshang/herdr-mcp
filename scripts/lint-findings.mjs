#!/usr/bin/env node

// Deterministic gate for the FINDINGS.md -> RULES.md promotion policy in AGENTS.md.
//
// - `Seen N×` must equal the number of DISTINCT review dates listed on that line.
// - Review dates must be real `YYYY-MM-DD` dates and must not be listed twice.
// - An entry with >= 3 distinct review dates must already live in RULES.md.
// - The same one-line headline appears at most once in FINDINGS.md and never in both files.
// - Every entry carries a bold headline and a `Seen N× (...)` line.
//
// `--candidates` never fails: it lists entries at exactly 2 distinct review dates as
// upcoming promotions and entries at >= 3 as required promotions, for the daily reviewer.

import { readFileSync } from "node:fs";
import { basename, dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const PROMOTION_THRESHOLD = 3;
const SEPARATOR = "---";

const SEEN_LINE = /^\s*(?:[-*+]\s+)?Seen\s+(\d+)\s*[\u00d7xX]\s*\((.*)\)\s*\.?\s*$/;
const HEADLINE_LINE = /^\s*\*\*(.+)\*\*\s*$/;
const ISO_DATE = /^(\d{4})-(\d{2})-(\d{2})$/;

const USAGE = [
  "USAGE: node scripts/lint-findings.mjs [--candidates] [--findings <path>] [--rules <path>]",
  "",
  "  (no flags)     fail when the lesson logs break the promotion policy",
  "  --candidates   list upcoming (2 dates) and required (3+ dates) promotions",
].join("\n");

function parseArgs(argv) {
  const options = {
    candidates: false,
    findings: resolve(REPO_ROOT, "FINDINGS.md"),
    rules: resolve(REPO_ROOT, "RULES.md"),
  };

  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];

    if (arg === "--candidates") {
      options.candidates = true;
    } else if (arg === "--findings" || arg === "--rules") {
      const value = argv[index + 1];
      if (!value) {
        console.error("findings-lint: " + arg + " needs a path\n" + USAGE);
        process.exit(2);
      }
      index += 1;
      options[arg === "--findings" ? "findings" : "rules"] = resolve(value);
    } else if (arg === "-h" || arg === "--help") {
      console.log(USAGE);
      process.exit(0);
    } else {
      console.error("findings-lint: unknown argument " + JSON.stringify(arg) + "\n" + USAGE);
      process.exit(2);
    }
  }

  return options;
}

function isCalendarDate(value) {
  const match = value.match(ISO_DATE);
  if (!match) return false;
  const year = Number(match[1]);
  const month = Number(match[2]);
  const day = Number(match[3]);
  if (month < 1 || month > 12 || day < 1) return false;
  const date = new Date(Date.UTC(year, month - 1, day));
  return date.getUTCFullYear() === year && date.getUTCMonth() === month - 1 && date.getUTCDate() === day;
}

function normalizeHeadline(value) {
  return value.replace(/\s+/g, " ").trim();
}

function quote(entry) {
  return JSON.stringify(entry.headline);
}

function parseDocument(path) {
  const label = basename(path);
  const text = readFileSync(path, "utf8");
  const lines = text.split(/\r?\n/);
  const separatorIndex = lines.findIndex((line) => line.trim() === SEPARATOR);
  // Everything above the first horizontal rule is the preamble (title + template).
  const body = separatorIndex < 0 ? lines : lines.slice(separatorIndex + 1);

  const entries = [];
  const orphans = [];
  let current = null;
  let loose = [];

  const flushLoose = () => {
    for (const line of loose) {
      if (SEEN_LINE.test(line)) orphans.push(line.trim());
    }
    loose = [];
  };

  for (const line of body) {
    if (line.trim() === SEPARATOR) continue;

    const headline = line.match(HEADLINE_LINE);
    if (headline) {
      flushLoose();
      current = { label, headline: normalizeHeadline(headline[1]), lines: [] };
      entries.push(current);
      continue;
    }

    if (current) current.lines.push(line);
    else loose.push(line);
  }

  flushLoose();
  return { label, entries, orphans };
}

function inspectEntry(entry) {
  const seen = entry.lines.map((line) => line.match(SEEN_LINE)).filter(Boolean);

  if (seen.length === 0) {
    return { issues: ["missing `- Seen N× (YYYY-MM-DD, ...).` line"], count: null };
  }
  if (seen.length > 1) {
    return { issues: ["more than one `Seen` line; keep one line listing every distinct review date"], count: null };
  }

  const stated = Number(seen[0][1]);
  const dates = seen[0][2].split(/[\s,;]+/).filter(Boolean);
  const issues = [];

  if (dates.length === 0) issues.push("no review dates listed");

  const invalid = dates.filter((date) => !isCalendarDate(date));
  if (invalid.length > 0) {
    issues.push("invalid review date(s) " + invalid.join(", ") + " (must be real YYYY-MM-DD dates)");
  }

  const duplicates = [...new Set(dates.filter((date, index) => dates.indexOf(date) !== index))];
  if (duplicates.length > 0) {
    issues.push("duplicate review date(s) listed more than once: " + duplicates.join(", "));
  }

  const distinct = new Set(dates).size;
  if (stated !== distinct) {
    issues.push("`Seen " + stated + "×` but " + distinct + " distinct review date(s) are listed");
  }

  return { issues, count: distinct };
}

function collectIssues(findingsDoc, rulesDoc) {
  const issues = [];

  for (const doc of [findingsDoc, rulesDoc]) {
    for (const entry of doc.entries) {
      for (const issue of inspectEntry(entry).issues) {
        issues.push(doc.label + ": " + quote(entry) + " — " + issue);
      }
    }
    for (const orphan of doc.orphans) {
      issues.push(doc.label + ": " + JSON.stringify(orphan) + " — `Seen` line is not attached to a bold headline entry");
    }
  }

  const byHeadline = new Map();
  for (const doc of [findingsDoc, rulesDoc]) {
    for (const entry of doc.entries) {
      const occurrences = byHeadline.get(entry.headline) ?? [];
      occurrences.push(doc.label);
      byHeadline.set(entry.headline, occurrences);
    }
  }

  for (const [headline, labels] of byHeadline) {
    if (labels.length < 2) continue;
    const inFindings = labels.includes("FINDINGS.md");
    const inRules = labels.includes("RULES.md");
    const reason =
      inFindings && inRules
        ? "headline appears in both FINDINGS.md and RULES.md; a promoted lesson must be removed from FINDINGS.md"
        : "headline appears " + labels.length + "× in " + [...new Set(labels)].join(" / ") + "; edit the existing entry instead of adding a duplicate";
    issues.push("FINDINGS.md: " + JSON.stringify(headline) + " — " + reason);
  }

  for (const entry of findingsDoc.entries) {
    const { count } = inspectEntry(entry);
    if (count !== null && count >= PROMOTION_THRESHOLD) {
      issues.push(
        findingsDoc.label +
          ": " +
          quote(entry) +
          " — " +
          count +
          " distinct review dates; promote it to RULES.md in this change",
      );
    }
  }

  return issues;
}

function summary(findingsDoc, rulesDoc) {
  const count = (doc) => doc.entries.length + (doc.entries.length === 1 ? " entry" : " entries");
  return findingsDoc.label + " " + count(findingsDoc) + ", " + rulesDoc.label + " " + count(rulesDoc);
}

function printSection(title, rows) {
  console.log("  " + title + ":");
  if (rows.length === 0) {
    console.log("    none");
    return;
  }
  for (const row of rows) {
    console.log("    - " + row.count + "× " + row.entry.headline);
  }
}

function printCandidates(findingsDoc, rulesDoc) {
  const rows = findingsDoc.entries.map((entry) => ({ entry, ...inspectEntry(entry) }));
  const upcoming = rows.filter((row) => row.count === 2);
  const required = rows.filter((row) => row.count !== null && row.count >= PROMOTION_THRESHOLD);
  const unreadable = rows.filter((row) => row.count === null);

  console.log("findings-lint: promotion candidates (" + summary(findingsDoc, rulesDoc) + ")");
  printSection("upcoming (2 distinct review dates; promote on the next hit)", upcoming);
  printSection(
    "required now (" + PROMOTION_THRESHOLD + "+ distinct review dates; the gate fails until moved to RULES.md)",
    required,
  );
  if (unreadable.length > 0) printSection("unreadable (fix the template before promoting)", unreadable);

  console.log(
    required.length === 0
      ? "findings-lint: nothing is due for promotion"
      : "findings-lint: " +
          required.length +
          " entr" +
          (required.length === 1 ? "y" : "ies") +
          " must be promoted (candidates mode does not fail)",
  );
}

const options = parseArgs(process.argv.slice(2));

let findingsDoc;
let rulesDoc;
try {
  findingsDoc = parseDocument(options.findings);
  rulesDoc = parseDocument(options.rules);
} catch (error) {
  console.error("findings-lint: FAIL");
  console.error("- " + error.message);
  process.exit(1);
}

if (options.candidates) {
  printCandidates(findingsDoc, rulesDoc);
  process.exit(0);
}

const issues = collectIssues(findingsDoc, rulesDoc);

if (issues.length > 0) {
  console.error("findings-lint: FAIL (" + summary(findingsDoc, rulesDoc) + ")");
  for (const issue of issues) console.error("- " + issue);
  process.exit(1);
}

console.log(
  "findings-lint: PASS (" + summary(findingsDoc, rulesDoc) + "; no entry has reached " + PROMOTION_THRESHOLD + " distinct review dates)",
);
