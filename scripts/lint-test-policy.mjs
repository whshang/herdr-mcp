#!/usr/bin/env node

import { execFileSync } from "node:child_process";

const requestedBase = process.argv[2]?.trim();

function git(args) {
  return execFileSync("git", args, { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] }).trim();
}

function resolveBase() {
  if (requestedBase) {
    try {
      git(["cat-file", "-e", requestedBase + "^{commit}"]);
      return requestedBase;
    } catch {
      // Shallow push checkouts can omit the event's before SHA.
    }
  }
  try {
    return git(["rev-parse", "HEAD^"]);
  } catch {
    return null;
  }
}

const base = resolveBase();
if (!base) {
  console.log("test-policy: no parent commit; nothing to lint");
  process.exit(0);
}

const diff = execFileSync(
  "git",
  [
    "diff",
    "--unified=0",
    "--no-color",
    base,
    "HEAD",
    "--",
    "tests/**/*.mjs",
    "tests/*.mjs",
    "edge/cloudflare/tests/*.mjs",
  ],
  { encoding: "utf8" },
);

const violations = [];
let file = "";

for (const rawLine of diff.split("\n")) {
  if (rawLine.startsWith("+++ b/")) {
    file = rawLine.slice(6);
    continue;
  }
  if (!rawLine.startsWith("+") || rawLine.startsWith("+++")) continue;

  const line = rawLine.slice(1);
  const titleMatch = line.match(/\btest\s*\(\s*([\"'])(.+?)\1/);
  if (titleMatch) {
    const title = titleMatch[2];
    const given = title.indexOf("Given ");
    const when = title.indexOf("When ");
    const then = title.indexOf("Then ");
    if (!title.startsWith("user ") || given < 0 || when <= given || then <= when) {
      violations.push(
        file + ': new test title must start with "user " and describe Given / When / Then in order: ' + title,
      );
    }
  }

  if (
    /\b(?:jest|vi)\.mock\b/.test(line) ||
    /\bsinon\b/.test(line) ||
    /\bt\.mock\./.test(line) ||
    /\bmock\.(?:fn|method|module|timers)\b/.test(line)
  ) {
    violations.push(
      file + ": mocking APIs are not allowed; use the real dependency or a contract-checked fake",
    );
  }
}

if (violations.length > 0) {
  console.error("test-policy: FAIL");
  for (const violation of violations) console.error("- " + violation);
  process.exit(1);
}

console.log("test-policy: PASS base=" + base);
