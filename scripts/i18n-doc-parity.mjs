// Documentation translation parity checker.
//
// English under docs/i18n/en is the canonical technical source. This module
// extracts the parts of a document that must survive translation verbatim —
// heading depth, fenced-block sequence, shell command lines, machine-readable
// inline code, numeric/version literals and link targets — so a translated
// variant can be compared against the canonical source.
//
// It exists so a translated page cannot silently drop a command, an identifier,
// a security constant or a version claim. Prose is expected to differ; nothing
// here asserts translated wording.
//
// CLI: node scripts/i18n-doc-parity.mjs [locale] [slug ...]
// Prints one finding block per document that diverges from the canonical source.

import { readFile, readdir } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { join } from "node:path";

const rootPath = fileURLToPath(new URL("../", import.meta.url));

// Fenced blocks whose content is executable rather than prose. Prompt/example
// blocks keep their info string (usually `text`) but their body is translated.
const COMMAND_FENCE_INFOS = new Set(["bash", "sh", "shell", "zsh", "console", "powershell", "ps1", "cmd"]);

const CJK = /[\u3040-\u30ff\u3400-\u4dbf\u4e00-\u9fff\uac00-\ud7af]/;

export function parseDocStructure(source) {
  const lines = source.split(/\r?\n/);
  const headings = [];
  const fences = [];
  const inlineCode = [];
  const links = [];
  let openFenceChar = null;
  let fenceInfo = "";
  let buffer = [];

  const closable = (line) => {
    if (!openFenceChar) return false;
    const marker = openFenceChar === "`" ? "`" : "~";
    return new RegExp(`^\\s*${marker}{3,}\\s*$`).test(line);
  };

  for (const line of lines) {
    if (!openFenceChar) {
      const opener = line.match(/^\s*(`{3,}|~{3,})\s*([^\s`]*)/);
      if (opener) {
        openFenceChar = opener[1][0];
        fenceInfo = (opener[2] || "").toLowerCase();
        buffer = [];
        continue;
      }
      const heading = line.match(/^(#{1,6})\s+(.*?)\s*$/);
      if (heading) headings.push({ depth: heading[1].length, title: heading[2] });
      for (const match of line.matchAll(/`([^`\n]+)`/g)) inlineCode.push(match[1]);
      for (const match of line.matchAll(/\]\(([^)\s]+)\)/g)) links.push(match[1]);
      continue;
    }
    if (closable(line)) {
      fences.push({ info: fenceInfo, lines: buffer });
      openFenceChar = null;
      fenceInfo = "";
      buffer = [];
      continue;
    }
    buffer.push(line);
  }
  if (openFenceChar) fences.push({ info: fenceInfo, lines: buffer });
  return { headings, fences, inlineCode, links };
}

// Quoted arguments are frequently human-readable example values (a device
// label, a task description) that a translation may legitimately localize, so
// only the command structure is compared: quoted payloads collapse to an empty
// pair of quotes on both sides of the comparison.
export function commandLines(fences) {
  const commands = new Set();
  for (const fence of fences) {
    if (!COMMAND_FENCE_INFOS.has(fence.info)) continue;
    for (const line of fence.lines) {
      const trimmed = line.trim();
      if (!trimmed || trimmed.startsWith("#") || trimmed.startsWith("//")) continue;
      commands.add(trimmed.replace(/"[^"]*"/g, '""').replace(/'[^']*'/g, "''"));
    }
  }
  return commands;
}

// Inline code that carries machine meaning: an identifier, path, URL, command,
// JSON key or `key=value` pair. Pure-prose spans and CJK spans are ignored.
export function machineInlineCode(spans) {
  const tokens = new Set();
  for (const span of spans) {
    const trimmed = span.trim();
    if (!trimmed || CJK.test(trimmed) || /\s/.test(trimmed)) continue;
    if (!/[A-Za-z0-9]/.test(trimmed)) continue;
    tokens.add(trimmed);
  }
  return tokens;
}

// In-page anchors are localized (heading text is translated, so the generated id
// differs per locale) and are therefore excluded from link-target parity.
export function linkTargets(links) {
  return new Set(links.filter((target) => !target.startsWith("#")));
}

// Versions, ports, byte/ms budgets, store permission modes, epoch numbers.
export function numericLiterals(source) {
  const tokens = new Set();
  for (const match of source.matchAll(/\bv\d+(?:\.\d+)+/g)) tokens.add(match[0]);
  for (const match of source.matchAll(/(?<![\w.`-])\d[\d_]*(?:\.\d+)+(?![\w`])/g)) tokens.add(match[0].replaceAll("_", ""));
  for (const match of source.matchAll(/(?<![\w.`-])\d{2,}(?![\w`])/g)) tokens.add(match[0].replaceAll("_", ""));
  return tokens;
}

function missingFrom(expected, actual) {
  return [...expected].filter((token) => !actual.has(token));
}

/** Compare one translated document against its canonical source. */
export function compareDocument(canonicalSource, translatedSource) {
  const canonical = parseDocStructure(canonicalSource);
  const translated = parseDocStructure(translatedSource);
  const findings = [];

  const canonicalHeadings = canonical.headings.map((heading) => heading.depth).join(",");
  const translatedHeadings = translated.headings.map((heading) => heading.depth).join(",");
  if (canonicalHeadings !== translatedHeadings) {
    findings.push({ kind: "heading-structure", detail: `canonical [${canonicalHeadings}] vs translated [${translatedHeadings}]` });
  }

  const canonicalFences = canonical.fences.map((fence) => fence.info).join("|");
  const translatedFences = translated.fences.map((fence) => fence.info).join("|");
  if (canonicalFences !== translatedFences) {
    findings.push({ kind: "fence-sequence", detail: `canonical [${canonicalFences}] vs translated [${translatedFences}]` });
  }

  const missingCommands = missingFrom(commandLines(canonical.fences), commandLines(translated.fences));
  if (missingCommands.length) findings.push({ kind: "missing-command", detail: missingCommands.join(" | ") });

  const missingCode = missingFrom(machineInlineCode(canonical.inlineCode), machineInlineCode(translated.inlineCode));
  if (missingCode.length) findings.push({ kind: "missing-inline-code", detail: missingCode.join(" | ") });

  const missingLinks = missingFrom(linkTargets(canonical.links), linkTargets(translated.links));
  if (missingLinks.length) findings.push({ kind: "missing-link-target", detail: missingLinks.join(" | ") });

  const missingNumbers = missingFrom(numericLiterals(canonicalSource), numericLiterals(translatedSource));
  if (missingNumbers.length) findings.push({ kind: "missing-numeric-literal", detail: missingNumbers.join(" | ") });

  return findings;
}

/** Compare every document of one locale against the canonical English source. */
export async function checkLocale({ root = rootPath, locale, slugs = null, canonical = "en" } = {}) {
  const canonicalDir = join(root, "docs", "i18n", canonical);
  const translatedDir = join(root, "docs", "i18n", locale);
  const names = slugs ?? (await readdir(translatedDir)).filter((name) => name.endsWith(".md")).sort();
  const report = new Map();
  for (const name of names) {
    const slug = name.replace(/\.md$/, "");
    let translatedSource;
    try {
      translatedSource = await readFile(join(translatedDir, `${slug}.md`), "utf8");
    } catch {
      report.set(slug, [{ kind: "missing-document", detail: `${locale}/${slug}.md is absent` }]);
      continue;
    }
    const canonicalSource = await readFile(join(canonicalDir, `${slug}.md`), "utf8");
    const findings = compareDocument(canonicalSource, translatedSource);
    if (findings.length) report.set(slug, findings);
  }
  return report;
}

export function formatReport(locale, report) {
  const lines = [];
  for (const [slug, findings] of report) {
    lines.push(`[${locale}] ${slug}`);
    for (const finding of findings) lines.push(`  ${finding.kind}: ${finding.detail}`);
  }
  return lines.join("\n");
}

if (process.argv[1] && import.meta.url === `file://${process.argv[1]}`) {
  const [locale = "ja", ...slugs] = process.argv.slice(2);
  const report = await checkLocale({ locale, slugs: slugs.length ? slugs.map((slug) => `${slug}.md`) : null });
  if (report.size === 0) console.log(`${locale}: parity checks pass`);
  else {
    console.log(formatReport(locale, report));
    process.exitCode = 1;
  }
}