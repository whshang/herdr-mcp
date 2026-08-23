#!/usr/bin/env node
import { access, cp, mkdir, readFile, readdir, rm, writeFile } from "node:fs/promises";
import { basename, extname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { marked } from "marked";

const rootPath = fileURLToPath(new URL("../", import.meta.url));
const siteDir = join(rootPath, "site");
const docsDir = join(rootPath, "docs");
const outDir = join(rootPath, "site-dist");

const NAV_GROUPS = [
  { label: "Start here", slugs: ["architecture", "chatgpt-connector"] },
  { label: "ChatGPT & Web", slugs: ["worker-fallbacks"] },
  {
    label: "Operations & Deployment",
    slugs: ["cloudflare-edge-deployment", "cloudflare-edge-token", "runtime-self-upgrade", "automation"],
  },
  { label: "Browser Extension", slugs: ["extension", "extension-wake", "extension-bridge"] },
  { label: "Reference & Development", slugs: ["capability-benchmark"] },
];
const NAV_ORDER = NAV_GROUPS.flatMap((group) => group.slugs);

function esc(value) {
  return String(value)
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;");
}

function jsonForScript(value) {
  return JSON.stringify(value).replaceAll("<", "\\u003c").replaceAll(">", "\\u003e").replaceAll("&", "\\u0026");
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

function headingSlug(text) {
  return String(text)
    .toLowerCase()
    .replace(/<[^>]*>/g, "")
    .replace(/[`*_~]/g, "")
    .replace(/[^\p{L}\p{N}\s-]/gu, "")
    .trim()
    .replace(/\s+/g, "-")
    .replace(/-+/g, "-") || "section";
}

function plainHeading(text) {
  return String(text).replace(/[`*_~]/g, "").replace(/<[^>]*>/g, "").trim();
}

function renderMarkdown(source) {
  const toc = [];
  const seen = new Map();
  const renderer = new marked.Renderer();
  renderer.heading = function heading(token) {
    const inline = this.parser.parseInline(token.tokens);
    if (token.depth !== 2 && token.depth !== 3) {
      return `<h${token.depth}>${inline}</h${token.depth}>\n`;
    }
    const base = headingSlug(token.text);
    const count = seen.get(base) ?? 0;
    seen.set(base, count + 1);
    const id = count === 0 ? base : `${base}-${count + 1}`;
    toc.push({ depth: token.depth, id, title: plainHeading(token.text) });
    return `<h${token.depth} id="${esc(id)}">${inline}<a class="heading-anchor" href="#${esc(id)}" aria-label="Link to ${esc(plainHeading(token.text))}">#</a></h${token.depth}>\n`;
  };
  const html = marked.parse(source, { gfm: true, renderer });
  return { html: rewriteDocLinks(html), toc };
}

function topbar({ drawer = false } = {}) {
  return `<header class="topbar${drawer ? " has-drawer" : ""}">
    ${drawer ? `<button class="nav-toggle" type="button" data-nav-toggle aria-label="Open documentation navigation" aria-controls="docs-sidebar" aria-expanded="false"><span aria-hidden="true">☰</span></button>` : ""}
    <a class="brand" href="../">herdr-mcp</a>
    <nav class="topnav" aria-label="Primary"><a href="./" aria-current="location">Docs</a><a href="https://github.com/whshang/herdr-mcp">GitHub</a></nav>
    <div class="topbar-actions">
      <button class="search-trigger" type="button" data-search-open aria-haspopup="dialog"><span aria-hidden="true">⌕</span><span class="search-label">Search</span><kbd>⌘K</kbd></button>
      <button class="theme-toggle" type="button" data-theme-toggle aria-label="Toggle color theme"><span data-theme-icon aria-hidden="true">◐</span></button>
    </div>
  </header>`;
}

function sidebarNav(docsBySlug, activeSlug = "") {
  return `<nav class="sidebar-nav" aria-label="Documentation">
    ${NAV_GROUPS.map((group) => `<section class="nav-group"><h2>${esc(group.label)}</h2><ul>${group.slugs.map((slug) => {
      const doc = docsBySlug.get(slug);
      const active = slug === activeSlug;
      return `<li><a data-doc-slug="${esc(slug)}" href="./${esc(slug)}.html"${active ? ' aria-current="page" class="active"' : ""}>${esc(doc.title)}</a></li>`;
    }).join("")}</ul></section>`).join("")}
  </nav>`;
}

function tocMarkup(toc) {
  if (!toc.length) return `<p class="toc-empty">No sections</p>`;
  return `<ol>${toc.map((item) => `<li class="toc-depth-${item.depth}"><a href="#${esc(item.id)}">${esc(item.title)}</a></li>`).join("")}</ol>`;
}

function searchUi(searchIndex) {
  return `<dialog class="search-dialog" data-search-dialog aria-labelledby="search-title">
    <div class="search-panel">
      <div class="search-heading"><div><span class="eyebrow">Documentation</span><h2 id="search-title">Search herdr-mcp</h2></div><button type="button" class="icon-button" data-search-close aria-label="Close search">×</button></div>
      <label class="search-input-wrap"><span class="sr-only">Search documentation</span><input type="search" data-search-input autocomplete="off" placeholder="Search pages and sections…"></label>
      <div class="search-results" data-search-results aria-live="polite"><p class="search-hint">Type to search the documentation.</p></div>
    </div>
  </dialog>
  <script id="search-index" type="application/json">${jsonForScript(searchIndex)}</script>
  <script src="../app.js" defer></script>`;
}

function pageNav(docsBySlug, slug) {
  const index = NAV_ORDER.indexOf(slug);
  const prev = index > 0 ? docsBySlug.get(NAV_ORDER[index - 1]) : null;
  const next = index >= 0 && index < NAV_ORDER.length - 1 ? docsBySlug.get(NAV_ORDER[index + 1]) : null;
  return `<nav class="page-nav" aria-label="Adjacent documentation pages">
    ${prev ? `<a class="page-nav-link prev" data-prev href="./${esc(prev.slug)}.html"><span>Previous</span><strong>← ${esc(prev.title)}</strong></a>` : `<span></span>`}
    ${next ? `<a class="page-nav-link next" data-next href="./${esc(next.slug)}.html"><span>Next</span><strong>${esc(next.title)} →</strong></a>` : `<span></span>`}
  </nav>`;
}

function articleShell(doc, body, toc, docsBySlug, searchIndex) {
  return `<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width,initial-scale=1">
  <meta name="description" content="${esc(doc.description)}">
  <meta name="color-scheme" content="light dark">
  <title>${esc(doc.title)} · herdr-mcp</title>
  <link rel="stylesheet" href="../style.css">
</head>
<body class="docs-page">
  ${topbar({ drawer: true })}
  <div class="nav-overlay" data-nav-overlay hidden></div>
  <div class="docs-layout">
    <aside class="sidebar" id="docs-sidebar" data-sidebar>${sidebarNav(docsBySlug, doc.slug)}</aside>
    <main class="article-column">
      <article class="doc-body" data-doc-slug="${esc(doc.slug)}">${body}</article>
      ${pageNav(docsBySlug, doc.slug)}
      <footer class="docs-footer"><a href="./">Documentation index</a><a href="https://github.com/whshang/herdr-mcp/blob/main/docs/${esc(doc.slug)}.md">Edit source</a></footer>
    </main>
    <aside class="toc" aria-label="On this page"><div class="toc-inner"><h2>On this page</h2>${tocMarkup(toc)}</div></aside>
  </div>
  ${searchUi(searchIndex)}
</body>
</html>`;
}

function docsIndexShell(docsBySlug, searchIndex) {
  const groups = NAV_GROUPS.map((group) => `<section class="index-group" data-nav-group="${esc(group.label)}"><div class="index-group-heading"><span class="eyebrow">${esc(group.label)}</span></div><div class="docs-grid">${group.slugs.map((slug) => {
    const doc = docsBySlug.get(slug);
    return `<article class="doc-card" data-doc-slug="${esc(slug)}"><h2><a href="./${esc(slug)}.html">${esc(doc.title)}</a></h2><p>${esc(doc.description)}</p></article>`;
  }).join("")}</div></section>`).join("");

  return `<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width,initial-scale=1">
  <meta name="description" content="herdr-mcp documentation: connect web planners to a local Herdr workstation through a stable MCP control plane.">
  <meta name="color-scheme" content="light dark">
  <title>Documentation · herdr-mcp</title>
  <link rel="stylesheet" href="../style.css">
</head>
<body class="docs-index-page">
  ${topbar()}
  <main class="docs-index">
    <header class="docs-hero">
      <span class="eyebrow">Documentation</span>
      <h1>Remote planning, local execution.</h1>
      <p>herdr-mcp is the remote control plane between ChatGPT or another web planner and a local Herdr workstation. Start with the system boundary, connect ChatGPT, then choose the deployment and browser workflows you need.</p>
      <div class="hero-actions"><a class="button primary" href="./chatgpt-connector.html">Connect ChatGPT</a><a class="button" href="./architecture.html">Architecture</a><a class="button" href="./cloudflare-edge-deployment.html">Deploy the Edge</a></div>
    </header>
    <section class="docs-paths" aria-label="Documentation paths">${groups}</section>
    <footer class="docs-footer"><a href="../">Home</a><a href="https://github.com/whshang/herdr-mcp">Source</a></footer>
  </main>
  ${searchUi(searchIndex)}
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
  docs.push({
    slug,
    source,
    title: titleFromMarkdown(source, slug),
    description: firstParagraph(source) || "herdr-mcp documentation",
  });
}

const docsBySlug = new Map(docs.map((doc) => [doc.slug, doc]));
const navSet = new Set(NAV_ORDER);
if (navSet.size !== NAV_ORDER.length) throw new Error("documentation navigation contains duplicate slugs");
const missing = NAV_ORDER.filter((slug) => !docsBySlug.has(slug));
const ungrouped = docs.map((doc) => doc.slug).filter((slug) => !navSet.has(slug));
if (missing.length || ungrouped.length) {
  throw new Error(`documentation navigation mismatch: missing=${missing.join(",") || "none"}; ungrouped=${ungrouped.join(",") || "none"}`);
}

const rendered = new Map();
for (const doc of docs) rendered.set(doc.slug, renderMarkdown(doc.source));
const searchIndex = NAV_ORDER.map((slug) => {
  const doc = docsBySlug.get(slug);
  const page = rendered.get(slug);
  return {
    title: doc.title,
    description: doc.description,
    href: `./${slug}.html`,
    headings: page.toc.map((item) => item.title),
  };
});

for (const slug of NAV_ORDER) {
  const doc = docsBySlug.get(slug);
  const page = rendered.get(slug);
  await writeFile(join(outDir, "docs", `${slug}.html`), articleShell(doc, page.html, page.toc, docsBySlug, searchIndex));
}
await writeFile(join(outDir, "docs", "index.html"), docsIndexShell(docsBySlug, searchIndex));

const pkg = JSON.parse(await readFile(join(rootPath, "package.json"), "utf8"));
// Explicit HERDR_SITE_COMMIT override wins over the ambient CI SHA; falling back
// to the working-tree marker only when neither is set.
const commit = process.env.HERDR_SITE_COMMIT || process.env.GITHUB_SHA || "working-tree";
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
