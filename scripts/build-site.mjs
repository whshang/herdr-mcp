#!/usr/bin/env node
import { access, cp, mkdir, readFile, readdir, rm, writeFile } from "node:fs/promises";
import { basename, extname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { marked } from "marked";

const rootPath = fileURLToPath(new URL("../", import.meta.url));
const siteDir = join(rootPath, "site");
const docsDir = join(rootPath, "docs");
const outDir = join(rootPath, "site-dist");

function esc(value) {
  return String(value)
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;");
}

function titleFromMarkdown(markdown, fallback) {
  const match = markdown.match(/^#\s+(.+)$/m);
  return match ? match[1].replace(/[`*_]/g, "").trim() : fallback;
}

function firstParagraph(markdown) {
  const lines = markdown
    .split(/\r?\n/)
    .filter((line) => !/^\s*(#|>|```|[-*+]\s|\d+\.\s|\|)/.test(line))
    .map((line) => line.trim());
  const chunks = [];
  for (const line of lines) {
    if (!line) {
      if (chunks.length) break;
      continue;
    }
    chunks.push(line);
    if (chunks.join(" ").length > 220) break;
  }
  const value = chunks.join(" ").replace(/[`*_\[\]]/g, "").replace(/\([^)]*\)/g, "");
  return value.length > 220 ? `${value.slice(0, 217)}...` : value;
}

function rewriteDocLinks(html) {
  return html
    .replace(/href="(\.\/)?([^"#?]+)\.md(#[^"]*)?"/g, (_m, _dot, p1, hash = "") => {
      if (p1.startsWith("../README")) return `href="https://github.com/whshang/herdr-mcp/blob/main/${p1.replace(/^\.\.\//, "")}${hash}"`;
      if (p1.startsWith("../")) return `href="https://github.com/whshang/herdr-mcp/blob/main/${p1.replace(/^\.\.\//, "")}.md${hash}"`;
      return `href="./${p1}.html${hash}"`;
    });
}

function shell(title, body, description = "herdr-mcp documentation") {
  return `<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width,initial-scale=1">
  <meta name="description" content="${esc(description)}">
  <title>${esc(title)} · herdr-mcp</title>
  <link rel="stylesheet" href="../style.css">
</head>
<body>
  <main class="docs-shell">
    <nav class="docs-nav">
      <a href="../">herdr-mcp</a>
      <a href="./">Docs</a>
      <a href="https://github.com/whshang/herdr-mcp">GitHub</a>
    </nav>
    <article class="doc-body">${body}</article>
    <footer>
      <a href="../">Home</a>
      <a href="./">Documentation index</a>
      <a href="https://github.com/whshang/herdr-mcp">Source</a>
    </footer>
  </main>
</body>
</html>`;
}

await rm(outDir, { recursive: true, force: true });
await mkdir(outDir, { recursive: true });
await cp(siteDir, outDir, { recursive: true });
await mkdir(join(outDir, "docs"), { recursive: true });

const entries = (await readdir(docsDir, { withFileTypes: true }))
  .filter((entry) => entry.isFile() && extname(entry.name) === ".md")
  .sort((a, b) => a.name.localeCompare(b.name));

const docs = [];
for (const entry of entries) {
  const source = await readFile(join(docsDir, entry.name), "utf8");
  const slug = basename(entry.name, ".md");
  const title = titleFromMarkdown(source, slug);
  const description = firstParagraph(source) || "herdr-mcp documentation";
  const body = rewriteDocLinks(await marked.parse(source, { gfm: true }));
  await writeFile(join(outDir, "docs", `${slug}.html`), shell(title, body, description));
  docs.push({ slug, title, description });
}

const cards = docs.map(({ slug, title, description }) => `
      <article class="doc-card">
        <h2><a href="./${esc(slug)}.html">${esc(title)}</a></h2>
        <p>${esc(description)}</p>
      </article>`).join("");

const docsIndex = `<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width,initial-scale=1">
  <meta name="description" content="herdr-mcp documentation index">
  <title>Documentation · herdr-mcp</title>
  <link rel="stylesheet" href="../style.css">
</head>
<body>
  <main class="docs-shell">
    <nav class="docs-nav"><a href="../">herdr-mcp</a><a href="./">Docs</a><a href="https://github.com/whshang/herdr-mcp">GitHub</a></nav>
    <header class="docs-header"><div class="eyebrow">Documentation</div><h1>herdr-mcp docs</h1><p>Architecture, Connector setup, Cloudflare deployment, browser wake, capability decisions and self-upgrade.</p></header>
    <section class="docs-grid">${cards}</section>
  </main>
</body>
</html>`;
await writeFile(join(outDir, "docs", "index.html"), docsIndex);

const pkg = JSON.parse(await readFile(join(rootPath, "package.json"), "utf8"));
const commit = process.env.GITHUB_SHA || process.env.HERDR_SITE_COMMIT || "working-tree";
const skillSource = join(rootPath, "assets", "herdr-mcp-SKILL.md");
let skill = null;
try {
  await access(skillSource);
  await cp(skillSource, join(outDir, "herdr-mcp-SKILL.md"));
  skill = "./herdr-mcp-SKILL.md";
} catch { /* skill ships with newer releases; Pages remains deployable without it */ }
await writeFile(join(outDir, "release.json"), `${JSON.stringify({
  name: pkg.name,
  version: pkg.version,
  commit,
  docs: "./docs/",
  skill,
}, null, 2)}\n`);
await writeFile(join(outDir, ".nojekyll"), "");

console.log(JSON.stringify({ ok: true, output: "site-dist", docs: docs.length, version: pkg.version, commit }));
