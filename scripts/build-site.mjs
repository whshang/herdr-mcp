#!/usr/bin/env node
import { access, cp, mkdir, readFile, readdir, rm, writeFile } from "node:fs/promises";
import { basename, extname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { marked } from "marked";
import {
  DEFAULT_LOCALE,
  DEFAULT_ORIGIN,
  DOC_ORDER,
  LOCALES,
  LOCALE_NAMES,
  NAV_GROUPS,
  NAV_GROUP_LABELS,
  SITE_ORIGIN_ENV,
  UI,
} from "./site-i18n.mjs";

const rootPath = fileURLToPath(new URL("../", import.meta.url));
const siteDir = join(rootPath, "site");
const i18nDocsDir = join(rootPath, "docs", "i18n");
const outDir = join(rootPath, "site-dist");

const origin = (process.env[SITE_ORIGIN_ENV] || DEFAULT_ORIGIN).replace(/\/+$/, "");

// Fail fast when someone adds a locale without translating every string.
const uiKeys = Object.keys(UI[LOCALES[0]]).slice().sort();
for (const locale of LOCALES) {
  const keys = Object.keys(UI[locale]).slice().sort();
  for (const key of uiKeys) if (!keys.includes(key)) throw new Error(`site-i18n UI missing key "${key}" for locale "${locale}"`);
  for (const key of keys) if (!uiKeys.includes(key)) throw new Error(`site-i18n UI has unknown key "${key}" for locale "${locale}"`);
}
if (NAV_GROUP_LABELS[DEFAULT_LOCALE].length !== NAV_GROUPS.length) {
  throw new Error(`NAV_GROUP_LABELS must have one label per NAV_GROUPS entry (locale=${DEFAULT_LOCALE})`);
}
const navLabelCounts = new Map(LOCALES.map((locale) => [locale, NAV_GROUP_LABELS[locale].length]));
for (const [locale, count] of navLabelCounts) {
  if (count !== NAV_GROUPS.length) throw new Error(`NAV_GROUP_LABELS must have one label per NAV_GROUPS entry (locale=${locale})`);
}

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

// Markdown stylesheet links inside a locale dir:
//   ./doc.md | doc.md           -> ./doc.html          (same logical doc, same locale)
//   ../../../(README|CHANGELOG).md -> GitHub blob link  (repo-root files)
// Any other ../ path is also treated as a repo-root reference.
function rewriteDocLinks(html) {
  return html.replace(/href="(\.\/)?([^"#?]+)\.md(#[^"]*)?"/g, (_m, _dot, p1, hash = "") => {
    let clean = p1;
    while (clean.startsWith("../")) clean = clean.slice(3);
    if (p1.startsWith("../")) {
      return `href="https://github.com/whshang/herdr-mcp/blob/main/${clean}${clean.endsWith(".md") ? "" : ".md"}${hash}"`;
    }
    return `href="./${clean}.html${hash}"`;
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

function headMeta({ pre, locale, canonicalHref, alternates, xDefault, description, title }) {
  const ui = UI[locale];
  const alternatesHtml = alternates
    .map(({ lang, href }) => `<link rel="alternate" hreflang="${lang}" href="${esc(href)}">`)
    .join("\n  ");
  return `  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width,initial-scale=1">
  <meta name="description" content="${esc(description)}">
  <meta name="color-scheme" content="light dark">
  <title>${esc(title)}</title>
  <link rel="canonical" href="${esc(canonicalHref)}">
  ${alternatesHtml}
  <link rel="alternate" hreflang="x-default" href="${esc(xDefault)}">
  <link rel="stylesheet" href="${pre}style.css">`;
}

// Language switcher: on article pages it maps the current slug to the other
// locale's same slug; on locale index pages it maps to the other locale's
// index; on the neutral chooser it maps to both locale indexes.
// localePrefix is the prefix from the current page directory to the docs
// root: "../" on /docs/<locale>/ pages, "" on the neutral /docs/ chooser.
function langSwitcher({ locale, localePrefix, slug = null, current = null, isChooser = false }) {
  const ui = UI[locale];
  const items = LOCALES.map((other) => {
    const href = isChooser ? `${other}/index.html` : slug ? `${localePrefix}${other}/${slug}.html` : `${localePrefix}${other}/index.html`;
    const active = isChooser ? false : current ? current === other : locale === other;
    return `<a href="${esc(href)}" hreflang="${esc(other)}" data-locale="${esc(other)}"${active ? ' aria-current="true"' : ""}>${esc(LOCALE_NAMES[other])}</a>`;
  });
  return `<nav class="lang-switcher" data-lang-switch aria-label="${esc(ui.langSwitcherAria)}">${items.join("")}</nav>`;
}

function topbar({ pre, locale, drawer = false, isChooser = false, slug = null, localePrefix }) {
  const ui = UI[locale];
  return `<header class="topbar${drawer ? " has-drawer" : ""}">
    ${drawer ? `<button class="nav-toggle" type="button" data-nav-toggle aria-label="${esc(ui.openNav)}" aria-controls="docs-sidebar" aria-expanded="false"><span aria-hidden="true">☰</span></button>` : ""}
    <a class="brand" href="${pre}" aria-label="${esc(ui.brandHomeAria)}">herdr-mcp</a>
    <nav class="topnav" aria-label="Primary">${isChooser ? "" : `<a href="./" aria-current="location">${esc(ui.docsNav)}</a>`}<a href="https://github.com/whshang/herdr-mcp">GitHub</a></nav>
    <div class="topbar-actions">
      ${langSwitcher({ locale, localePrefix, slug, current: isChooser ? null : locale, isChooser })}
      ${isChooser ? "" : `<button class="search-trigger" type="button" data-search-open aria-haspopup="dialog" aria-label="${esc(ui.searchTriggerAria)}"><span aria-hidden="true">⌕</span><span class="search-label">${esc(ui.searchLabel)}</span><kbd>⌘K</kbd></button>`}
      <button class="theme-toggle" type="button" data-theme-toggle aria-label="${esc(ui.themeToggleAria)}"><span data-theme-icon aria-hidden="true">◐</span></button>
    </div>
  </header>`;
}

function sidebarNav(docsBySlug, activeSlug = "", ui, locale) {
  const groups = NAV_GROUPS.map((group, index) => `<section class="nav-group"><h2>${esc(NAV_GROUP_LABELS[locale][index])}</h2><ul>${group.slugs.map((slug) => {
    const doc = docsBySlug.get(slug);
    const active = slug === activeSlug;
    return `<li><a data-doc-slug="${esc(slug)}" href="./${esc(slug)}.html"${active ? ' aria-current="page" class="active"' : ""}>${esc(doc.title)}</a></li>`;
  }).join("")}</ul></section>`).join("");
  return `<nav class="sidebar-nav" aria-label="${esc(ui.docsSidebarAria)}">
    ${groups}
  </nav>`;
}

function tocMarkup(toc, ui) {
  if (!toc.length) return `<p class="toc-empty">${esc(ui.tocEmpty)}</p>`;
  return `<ol>${toc.map((item) => `<li class="toc-depth-${item.depth}"><a href="#${esc(item.id)}">${esc(item.title)}</a></li>`).join("")}</ol>`;
}

function searchUi({ pre, locale, searchIndex }) {
  const ui = UI[locale];
  const i18n = {
    hint: ui.searchHint,
    noResults: ui.searchNoResults,
    openNav: ui.openNav,
    closeNav: ui.closeNav,
    themeToLight: ui.themeToLight,
    themeToDark: ui.themeToDark,
  };
  return `<dialog class="search-dialog" data-search-dialog aria-labelledby="search-title">
    <div class="search-panel">
      <div class="search-heading"><div><span class="eyebrow">${esc(ui.searchEyebrow)}</span><h2 id="search-title">${esc(ui.searchTitle)}</h2></div><button type="button" class="icon-button" data-search-close aria-label="${esc(ui.searchCloseAria)}">×</button></div>
      <label class="search-input-wrap"><span class="sr-only">${esc(ui.searchLabel)}</span><input type="search" data-search-input autocomplete="off" placeholder="${esc(ui.searchPlaceholder)}"></label>
      <div class="search-results" data-search-results aria-live="polite"><p class="search-hint">${esc(ui.searchHint)}</p></div>
    </div>
  </dialog>
  <script id="search-index" type="application/json">${jsonForScript(searchIndex)}</script>
  <script id="search-i18n" type="application/json">${jsonForScript(i18n)}</script>
  <script src="${pre}app.js" defer></script>`;
}

function pageNav(docsBySlug, slug, ui) {
  const index = DOC_ORDER.indexOf(slug);
  const prev = index > 0 ? docsBySlug.get(DOC_ORDER[index - 1]) : null;
  const next = index >= 0 && index < DOC_ORDER.length - 1 ? docsBySlug.get(DOC_ORDER[index + 1]) : null;
  return `<nav class="page-nav" aria-label="${esc(ui.pageNavAria)}">
    ${prev ? `<a class="page-nav-link prev" data-prev href="./${esc(prev.slug)}.html"><span>${esc(ui.previous)}</span><strong>← ${esc(prev.title)}</strong></a>` : `<span></span>`}
    ${next ? `<a class="page-nav-link next" data-next href="./${esc(next.slug)}.html"><span>${esc(ui.next)}</span><strong>${esc(next.title)} →</strong></a>` : `<span></span>`}
  </nav>`;
}

function articleShell({ locale, doc, body, toc, docsBySlug, searchIndex }) {
  const pre = "../../";
  const ui = UI[locale];
  const selfHref = `${origin}/docs/${locale}/${doc.slug}.html`;
  const alternates = LOCALES.map((lang) => ({ lang, href: `${origin}/docs/${lang}/${doc.slug}.html` }));
  const xDefault = `${origin}/docs/${DEFAULT_LOCALE}/${doc.slug}.html`;
  return `<!doctype html>
<html lang="${ui.htmlLang}">
<head>
  ${headMeta({ pre, locale, canonicalHref: selfHref, alternates, xDefault, description: doc.description, title: `${doc.title} · herdr-mcp` })}
</head>
<body class="docs-page">
  ${topbar({ pre, locale, drawer: true, slug: doc.slug, localePrefix: "../" })}
  <div class="nav-overlay" data-nav-overlay hidden></div>
  <div class="docs-layout">
    <aside class="sidebar" id="docs-sidebar" data-sidebar>${sidebarNav(docsBySlug, doc.slug, ui, locale)}</aside>
    <main class="article-column">
      <article class="doc-body" data-doc-slug="${esc(doc.slug)}">${body}</article>
      ${pageNav(docsBySlug, doc.slug, ui)}
      <footer class="docs-footer"><a href="./">${esc(ui.docsIndex)}</a><a href="https://github.com/whshang/herdr-mcp/blob/main/docs/i18n/${esc(locale)}/${esc(doc.slug)}.md">${esc(ui.editSource)}</a></footer>
    </main>
    <aside class="toc" aria-label="${esc(ui.onThisPage)}"><div class="toc-inner"><h2>${esc(ui.onThisPage)}</h2>${tocMarkup(toc, ui)}</div></aside>
  </div>
  ${searchUi({ pre, locale, searchIndex })}
</body>
</html>`;
}

function docsIndexShell({ locale, docsBySlug, searchIndex }) {
  const pre = "../../";
  const ui = UI[locale];
  const groups = NAV_GROUPS.map((group, index) => `<section class="index-group" data-nav-group="${esc(NAV_GROUP_LABELS[locale][index])}"><div class="index-group-heading"><span class="eyebrow">${esc(NAV_GROUP_LABELS[locale][index])}</span></div><div class="docs-grid">${group.slugs.map((slug) => {
    const doc = docsBySlug.get(slug);
    return `<article class="doc-card" data-doc-slug="${esc(slug)}"><h2><a href="./${esc(slug)}.html">${esc(doc.title)}</a></h2><p>${esc(doc.description)}</p></article>`;
  }).join("")}</div></section>`).join("");
  const selfHref = `${origin}/docs/${locale}/index.html`;
  const alternates = LOCALES.map((lang) => ({ lang, href: `${origin}/docs/${lang}/index.html` }));
  return `<!doctype html>
<html lang="${ui.htmlLang}">
<head>
  ${headMeta({ pre, locale, canonicalHref: selfHref, alternates, xDefault: `${origin}/docs/index.html`, description: ui.indexLead, title: `${ui.docsNav} · herdr-mcp` })}
</head>
<body class="docs-index-page">
  ${topbar({ pre, locale, localePrefix: "../" })}
  <main class="docs-index">
    <header class="docs-hero">
      <span class="eyebrow">${esc(ui.indexEyebrow)}</span>
      <h1>${esc(ui.indexTitle)}</h1>
      <p>${esc(ui.indexLead)}</p>
      <div class="hero-actions"><a class="button primary" href="./chatgpt-connector.html">${esc(ui.indexCtaConnect)}</a><a class="button" href="./architecture.html">${esc(ui.indexCtaArchitecture)}</a><a class="button" href="./cloudflare-edge-deployment.html">${esc(ui.indexCtaDeploy)}</a></div>
    </header>
    <section class="docs-paths" aria-label="${esc(ui.indexEyebrow)}">${groups}</section>
    <footer class="docs-footer" aria-label="${esc(ui.indexFooterAria)}"><a href="${pre}">${esc(ui.indexHome)}</a><a href="https://github.com/whshang/herdr-mcp">${esc(ui.indexSource)}</a></footer>
  </main>
  ${searchUi({ pre, locale, searchIndex })}
</body>
</html>`;
}

// Neutral /docs/ entry: a bilingual chooser that must not flatten both
// languages into one navigation. It only links to the two locale indexes.
function docsChooserShell() {
  const pre = "../";
  const zh = UI[DEFAULT_LOCALE];
  const en = UI.en;
  return `<!doctype html>
<html lang="en">
<head>
  ${headMeta({
    pre,
    locale: DEFAULT_LOCALE,
    canonicalHref: `${origin}/docs/index.html`,
    alternates: LOCALES.map((lang) => ({ lang, href: `${origin}/docs/${lang}/index.html` })),
    xDefault: `${origin}/docs/index.html`,
    description: `${en.chooserLead} ${zh.chooserLead}`,
    title: `${en.chooserTitle} · herdr-mcp`,
  })}
</head>
<body class="docs-index-page docs-chooser-page">
  ${topbar({ pre, locale: DEFAULT_LOCALE, isChooser: true, localePrefix: "" })}
  <main class="docs-index docs-chooser">
    <header class="docs-hero">
      <span class="eyebrow">${esc(en.chooserEyebrow)} · ${esc(zh.chooserEyebrow)}</span>
      <h1>${esc(zh.chooserTitle)}<span class="chooser-divider">/</span>${esc(en.chooserTitle)}</h1>
      <p>${esc(en.chooserLead)} ${esc(zh.chooserLead)}</p>
    </header>
    <section class="chooser-grid" aria-label="${esc(en.chooserTitle)}">
      <article class="chooser-card"><h2><a href="./${DEFAULT_LOCALE}/index.html" hreflang="${DEFAULT_LOCALE}">${esc(zh.chooserZh)}</a></h2><p>${esc(zh.chooserZhBlurb)}</p></article>
      <article class="chooser-card"><h2><a href="./en/index.html" hreflang="en">${esc(en.chooserEn)}</a></h2><p>${esc(en.chooserEnBlurb)}</p></article>
    </section>
    <footer class="docs-footer"><a href="${pre}">${esc(en.chooserHome)}</a><a href="https://github.com/whshang/herdr-mcp">GitHub</a></footer>
  </main>
  <script src="${pre}app.js" defer></script>
</body>
</html>`;
}

await rm(outDir, { recursive: true, force: true });
await mkdir(outDir, { recursive: true });
await cp(siteDir, outDir, { recursive: true });
await mkdir(join(outDir, "docs"), { recursive: true });

const byLocale = new Map();
for (const locale of LOCALES) {
  const dir = join(i18nDocsDir, locale);
  const entries = (await readdir(dir, { withFileTypes: true }))
    .filter((entry) => entry.isFile() && extname(entry.name) === ".md")
    .sort((a, b) => a.name.localeCompare(b.name));
  const slugs = entries.map((entry) => basename(entry.name, ".md"));
  const duplicates = slugs.filter((slug, index) => slugs.indexOf(slug) !== index);
  const missing = DOC_ORDER.filter((slug) => !slugs.includes(slug));
  const extra = slugs.filter((slug) => !DOC_ORDER.includes(slug));
  if (duplicates.length || missing.length || extra.length) {
    throw new Error(
      `locale ${locale} must contain exactly the ${DOC_ORDER.length} logical docs: duplicates=${duplicates.join(",") || "none"}; missing=${missing.join(",") || "none"}; extra=${extra.join(",") || "none"}`
    );
  }
  const docs = [];
  for (const entry of entries) {
    const source = await readFile(join(dir, entry.name), "utf8");
    const slug = basename(entry.name, ".md");
    docs.push({
      slug,
      locale,
      source,
      title: titleFromMarkdown(source, slug),
      description: firstParagraph(source) || "herdr-mcp documentation",
    });
  }
  const docsBySlug = new Map(docs.map((doc) => [doc.slug, doc]));
  const rendered = new Map();
  for (const doc of docs) rendered.set(doc.slug, renderMarkdown(doc.source));
  const searchIndex = DOC_ORDER.map((slug) => {
    const doc = docsBySlug.get(slug);
    const page = rendered.get(slug);
    return {
      title: doc.title,
      description: doc.description,
      href: `./${slug}.html`,
      headings: page.toc.map((item) => item.title),
    };
  });
  byLocale.set(locale, { docsBySlug, rendered, searchIndex });

  const localeOut = join(outDir, "docs", locale);
  await mkdir(localeOut, { recursive: true });
  for (const slug of DOC_ORDER) {
    const doc = docsBySlug.get(slug);
    const page = rendered.get(slug);
    await writeFile(
      join(localeOut, `${slug}.html`),
      articleShell({ locale, doc, body: page.html, toc: page.toc, docsBySlug, searchIndex })
    );
  }
  await writeFile(join(localeOut, "index.html"), docsIndexShell({ locale, docsBySlug, searchIndex }));
}

await writeFile(join(outDir, "docs", "index.html"), docsChooserShell());

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

console.log(JSON.stringify({ ok: true, output: "site-dist", locales: LOCALES.map((l) => `${l}*${DOC_ORDER.length}`), docs: DOC_ORDER.length, version: pkg.version, commit }));