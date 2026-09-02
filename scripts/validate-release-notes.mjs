#!/usr/bin/env node

import { readFile } from "node:fs/promises";
import { pathToFileURL } from "node:url";

const TAG_RE = /^v\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.-]+)?$/;
const PR_RE = /https:\/\/github\.com\/whshang\/herdr-mcp\/pull\/\d+/;

function section(lines, headingPredicate) {
  const start = lines.findIndex((line) => /^##\s+/.test(line) && headingPredicate(line));
  if (start < 0) return null;
  let end = lines.length;
  for (let i = start + 1; i < lines.length; i += 1) {
    if (/^##\s+/.test(lines[i])) {
      end = i;
      break;
    }
  }
  return lines.slice(start + 1, end).join("\n").trim();
}

export function validateReleaseNotes(tag, text) {
  const errors = [];
  if (!TAG_RE.test(tag)) {
    errors.push(`invalid release tag: ${tag}`);
    return errors;
  }

  const lines = text.replace(/\r\n/g, "\n").split("\n");
  const expectedTitle = `# Herdr MCP ${tag}`;
  if (lines[0]?.trim() !== expectedTitle) {
    errors.push(`first line must be exactly: ${expectedTitle}`);
  }

  const upgrade = section(lines, (line) => /^##\s+Upgrade\b/i.test(line));
  const mainChanges = section(lines, (line) => /^##\s+Main changes\b/i.test(line));
  const knownIssues = section(lines, (line) => /^##\s+Known issues?\b/i.test(line));
  const compatibility = section(lines, (line) => /^##\s+Compatibility\b/i.test(line));

  if (!upgrade) {
    errors.push("missing non-empty `## Upgrade...` section");
  } else {
    if (!/\bherdr-mcp update(?: apply| check)?\b/.test(upgrade)) {
      errors.push("Upgrade section must contain the exact user upgrade command");
    }
    if (!/\bv\d+\.\d+\.\d+\b/.test(upgrade)) {
      errors.push("Upgrade section must name the previous/current version boundary");
    }
    if (!/\bherdr-mcp version\b/.test(upgrade) || !/\bherdr-mcp status\b/.test(upgrade)) {
      errors.push("Upgrade section must include `herdr-mcp version` and `herdr-mcp status` verification");
    }
  }

  if (!mainChanges) {
    errors.push("missing non-empty `## Main changes` section");
  } else {
    const bullets = mainChanges
      .split("\n")
      .map((line) => line.trim())
      .filter((line) => line.startsWith("- "));
    if (bullets.length === 0) {
      errors.push("Main changes must contain at least one bullet");
    }
    for (const bullet of bullets) {
      if (!PR_RE.test(bullet)) {
        errors.push(`each Main changes bullet must link its implementation PR: ${bullet}`);
      }
    }
  }

  if (!knownIssues) {
    errors.push("missing non-empty `## Known issue` / `## Known issues` section; use `- None known.` when empty");
  } else if (!knownIssues.split("\n").some((line) => line.trim().startsWith("- "))) {
    errors.push("Known issues section must contain at least one bullet; use `- None known.` when empty");
  }

  if (!compatibility) {
    errors.push("missing non-empty `## Compatibility` section");
  }

  return errors;
}

async function main() {
  const [tag, file = tag ? `docs/releases/${tag}.md` : null] = process.argv.slice(2);
  if (!tag || !file) {
    console.error("usage: node scripts/validate-release-notes.mjs <vX.Y.Z> [notes-file]");
    process.exitCode = 2;
    return;
  }
  let text;
  try {
    text = await readFile(file, "utf8");
  } catch (error) {
    console.error(`cannot read release notes ${file}: ${error.code ?? error.message}`);
    process.exitCode = 1;
    return;
  }
  const errors = validateReleaseNotes(tag, text);
  if (errors.length > 0) {
    for (const error of errors) console.error(`release-notes: ${error}`);
    process.exitCode = 1;
    return;
  }
  console.log(JSON.stringify({ ok: true, tag, file }));
}

if (import.meta.url === pathToFileURL(process.argv[1]).href) {
  await main();
}
