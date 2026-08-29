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
  READING_ORDER,
  SITE_ORIGIN_ENV,
  UI,
} from "./site-i18n.mjs";

const rootPath = fileURLToPath(new URL("../", import.meta.url));
const siteDir = join(rootPath, "site");
const i18nDocsDir = join(rootPath, "docs", "i18n");
const outDir = join(rootPath, "site-dist");

const origin = (process.env[SITE_ORIGIN_ENV] || DEFAULT_ORIGIN).replace(/\/+$/, "");
// package.json is build-tooling metadata, not the user-facing Runtime Release
// version. Keep the docs badge/release.json aligned with the authoritative Rust
// runtime version defined by crates/herdr-mcp/Cargo.toml.
const pkg = JSON.parse(await readFile(join(rootPath, "package.json"), "utf8"));
const cargoManifest = await readFile(join(rootPath, "crates", "herdr-mcp", "Cargo.toml"), "utf8");
const runtimeVersion = parseCargoPackageVersion(cargoManifest);

function parseCargoPackageVersion(source) {
  const packageSection = source.match(/(?:^|\n)\[package\]\s*\n([\s\S]*?)(?=\n\[|$)/)?.[1];
  const version = packageSection?.match(/^\s*version\s*=\s*"([^"]+)"\s*$/m)?.[1];
  if (!version) throw new Error("crates/herdr-mcp/Cargo.toml is missing [package].version");
  return version;
}

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
  <link rel="icon" type="image/png" href="${pre}favicon.png">
  <link rel="stylesheet" href="${pre}style.css">`;
}

// Language switcher: on article pages it maps the current slug to the other
// locale's same slug; on locale index pages it maps to the other locale's
// index. localePrefix is the prefix from the current page directory to the
// docs root: "../" on /docs/<locale>/ pages.
function langSwitcher({ locale, localePrefix, slug = null, current = null }) {
  const ui = UI[locale];
  const items = LOCALES.map((other) => {
    const href = slug ? `${localePrefix}${other}/${slug}.html` : `${localePrefix}${other}/index.html`;
    const active = current ? current === other : locale === other;
    return `<a href="${esc(href)}" hreflang="${esc(other)}" data-locale="${esc(other)}"${active ? ' aria-current="true"' : ""}>${esc(LOCALE_NAMES[other])}</a>`;
  });
  return `<nav class="lang-switcher" data-lang-switch aria-label="${esc(ui.langSwitcherAria)}">${items.join("")}</nav>`;
}

function topbar({ pre, locale, drawer = false, slug = null, localePrefix, version = null }) {
  const ui = UI[locale];
  const badge = version
    ? `<a class="version-badge" href="https://github.com/whshang/herdr-mcp/releases" aria-label="${esc(ui.versionBadgeAria)}: v${esc(version)}">${esc(ui.versionBadgeAria)} v${esc(version)}</a>`
    : "";
  return `<header class="topbar${drawer ? " has-drawer" : ""}">
    ${drawer ? `<button class="nav-toggle" type="button" data-nav-toggle aria-label="${esc(ui.openNav)}" aria-controls="docs-sidebar" aria-expanded="false"><span aria-hidden="true">☰</span></button>` : ""}
    <div class="brand-cell"><a class="brand" href="${pre}" aria-label="${esc(ui.brandHomeAria)}">herdr-mcp</a>${badge}</div>
    <nav class="topnav" aria-label="Primary"><a href="./" aria-current="location">${esc(ui.docsNav)}</a><a href="https://github.com/whshang/herdr-mcp">GitHub</a></nav>
    <div class="topbar-actions">
      ${langSwitcher({ locale, localePrefix, slug, current: locale })}
      <button class="search-trigger" type="button" data-search-open aria-haspopup="dialog" aria-label="${esc(ui.searchTriggerAria)}"><span aria-hidden="true">⌕</span><span class="search-label">${esc(ui.searchLabel)}</span><kbd>⌘K</kbd></button>
      <button class="theme-toggle" type="button" data-theme-toggle aria-label="${esc(ui.themeToggleAria)}"><span data-theme-icon aria-hidden="true">◐</span></button>
    </div>
  </header>`;
}

function sidebarNav(docsBySlug, activeSlug = "", ui, locale) {
  const groups = NAV_GROUPS.map((group, index) => `<section class="nav-group${group.secondary ? " nav-secondary" : ""}"><h2>${esc(NAV_GROUP_LABELS[locale][index])}</h2><ul>${group.slugs.map((slug) => {
    const doc = docsBySlug.get(slug);
    const active = slug === activeSlug;
    return `<li><a data-doc-slug="${esc(slug)}" href="./${esc(slug)}.html"${active ? ' aria-current="page" class="active"' : ""}>${esc(doc.title)}</a></li>`;
  }).join("")}</ul></section>`).join("");
  return `<nav class="sidebar-nav" aria-label="${esc(ui.docsSidebarAria)}">
    ${groups}
    <section class="nav-group nav-secondary"><h2>${esc(ui.historyNav)}</h2><ul><li><a href="https://github.com/whshang/herdr-mcp/tree/main/docs/history">${esc(ui.homeHistory)}</a></li></ul></section>
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
    copy: ui.copyCode,
    copied: ui.copiedCode,
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
  const index = READING_ORDER.indexOf(slug);
  if (index < 0) return "";
  const prev = index > 0 ? docsBySlug.get(READING_ORDER[index - 1]) : null;
  const next = index < READING_ORDER.length - 1 ? docsBySlug.get(READING_ORDER[index + 1]) : null;
  return `<nav class="page-nav" aria-label="${esc(ui.pageNavAria)}">
    ${prev ? `<a class="page-nav-link prev" data-prev href="./${esc(prev.slug)}.html"><span>${esc(ui.previous)}</span><strong>← ${esc(prev.title)}</strong></a>` : `<span></span>`}
    ${next ? `<a class="page-nav-link next" data-next href="./${esc(next.slug)}.html"><span>${esc(ui.next)}</span><strong>${esc(next.title)} →</strong></a>` : `<span></span>`}
  </nav>`;
}

function articleShell({ locale, doc, body, toc, docsBySlug, searchIndex, version }) {
  const pre = "../../";
  const ui = UI[locale];
  const selfHref = `${origin}/docs/${locale}/${doc.slug}.html`;
  const alternates = LOCALES.map((lang) => ({ lang, href: `${origin}/docs/${lang}/${doc.slug}.html` }));
  const xDefault = `${origin}/docs/${DEFAULT_LOCALE}/${doc.slug}.html`;
  // Copy button for every code block, matching the reference docs' code UI.
  const copyButton = `<button type="button" class="code-copy" data-copy-code aria-label="${esc(ui.copyCode)}">${esc(ui.copyCode)}</button>`;
  const bodyWithCopy = body.replace(/<pre(>|[ >])/g, (m, rest) => `<pre${rest}${copyButton}`);
  return `<!doctype html>
<html lang="${ui.htmlLang}">
<head>
  ${headMeta({ pre, locale, canonicalHref: selfHref, alternates, xDefault, description: doc.description, title: `${doc.title} · herdr-mcp` })}
</head>
<body class="docs-page">
  ${topbar({ pre, locale, drawer: true, slug: doc.slug, localePrefix: "../", version })}
  <div class="nav-overlay" data-nav-overlay hidden></div>
  <div class="docs-layout">
    <aside class="sidebar" id="docs-sidebar" data-sidebar>${sidebarNav(docsBySlug, doc.slug, ui, locale)}</aside>
    <main class="article-column">
      <article class="doc-body" data-doc-slug="${esc(doc.slug)}">${bodyWithCopy}</article>
      ${doc.slug === READING_ORDER[0] ? agentIntroBlock(ui, pre) : ""}
      ${pageNav(docsBySlug, doc.slug, ui)}
      <footer class="docs-footer"><a href="https://github.com/whshang/herdr-mcp/blob/main/docs/i18n/${esc(locale)}/${esc(doc.slug)}.md">${esc(ui.editSource)}</a></footer>
    </main>
    <aside class="toc" aria-label="${esc(ui.onThisPage)}"><div class="toc-inner"><h2>${esc(ui.onThisPage)}</h2>${tocMarkup(toc, ui)}</div></aside>
  </div>
  ${searchUi({ pre, locale, searchIndex })}
</body>
</html>`;
}

function agentIntroBlock(ui, pre) {
  return `<section class="agent-intro" data-agent-intro>
    <div class="agent-intro-head"><span class="eyebrow">👋</span><h2>${esc(ui.agentIntroTitle)}</h2><p>${esc(ui.agentIntroLead)}</p></div>
    <pre class="agent-prompt" tabindex="0"><code>${esc(ui.agentPrompt)}</code></pre>
    <p class="agent-intro-note"><a href="${pre}herdr-mcp-SKILL.md">${esc(ui.agentSkillLink)}</a></p>
  </section>`;
}

// Real locale documentation homepage. It is intentionally user-first: the
// primary path is Agent-assisted install, with the two human handoff points
// called out before architecture/maintainer material.
function localeEntryShell({ locale, docsBySlug, searchIndex, version }) {
  const pre = "../../";
  const ui = UI[locale];
  const selfHref = `${origin}/docs/${locale}/`;
  const alternates = LOCALES.map((lang) => ({ lang, href: `${origin}/docs/${lang}/` }));
  const xDefault = `${origin}/docs/${DEFAULT_LOCALE}/`;
  return `<!doctype html>
<html lang="${ui.htmlLang}">
<head>
  ${headMeta({ pre, locale, canonicalHref: selfHref, alternates, xDefault, description: ui.indexLead, title: `herdr-mcp ${ui.docsNav}` })}
</head>
<body class="docs-page docs-home-page">
  ${topbar({ pre, locale, drawer: true, localePrefix: "../", version })}
  <div class="nav-overlay" data-nav-overlay hidden></div>
  <div class="docs-layout">
    <aside class="sidebar" id="docs-sidebar" data-sidebar>${sidebarNav(docsBySlug, "", ui, locale)}</aside>
    <main class="article-column docs-home-column">
      <section class="docs-hero" id="start">
        <span class="eyebrow">${esc(ui.indexEyebrow)}</span>
        <h1>${esc(ui.indexTitle)}</h1>
        <p>${esc(ui.indexLead)}</p>
        <div class="hero-actions">
          <a class="button primary" href="./quick-agent-install.html">${esc(ui.indexCtaConnect)}</a>
          <a class="button" href="./install.html">${esc(ui.homeManualPathTitle)}</a>
          <a class="button" href="./overview.html">${esc(ui.indexCtaArchitecture)}</a>
        </div>
      </section>

      <section class="agent-intro" data-agent-intro id="agent-install">
        <div class="agent-intro-head"><span class="eyebrow">${esc(ui.homeAgentPathTitle)}</span><h2>${esc(ui.agentIntroTitle)}</h2><p>${esc(ui.agentIntroLead)}</p></div>
        <pre class="agent-prompt" tabindex="0"><button type="button" class="code-copy" data-copy-code aria-label="${esc(ui.copyCode)}">${esc(ui.copyCode)}</button><code>${esc(ui.agentPrompt)}</code></pre>
        <p class="agent-intro-note"><a href="./quick-agent-install.html">${esc(ui.agentSkillLink)}</a></p>
      </section>

      <div class="docs-paths">
        <section class="home-section" id="agent-does"><div class="index-group-heading"><span class="eyebrow">01</span><h2>${esc(ui.homeWillDoTitle)}</h2><p>${esc(ui.homeWillDoLead)}</p></div><div class="docs-grid home-three"><article class="doc-card"><h2>Herdr + herdr-mcp</h2><p>${esc(ui.homeWillDoHerdr)}</p></article><article class="doc-card"><h2>Edge + Link</h2><p>${esc(ui.homeWillDoEdge)}</p></article><article class="doc-card"><h2>Doctor</h2><p>${esc(ui.homeWillDoVerify)}</p></article></div></section>

        <section class="home-section" id="handoffs"><div class="index-group-heading"><span class="eyebrow">02</span><h2>${esc(ui.homeHandoffsTitle)}</h2></div><div class="docs-grid"><article class="doc-card handoff-card"><h2>${esc(ui.homeCloudflareTitle)}</h2><p>${esc(ui.homeCloudflareBody)}</p></article><article class="doc-card handoff-card"><h2>${esc(ui.homeChatgptTitle)}</h2><p>${esc(ui.homeChatgptBody)}</p></article></div></section>

        <section class="home-section" id="paths"><div class="index-group-heading"><span class="eyebrow">03</span><h2>${esc(ui.homePathsTitle)}</h2></div><div class="docs-grid home-three"><article class="doc-card"><h2><a href="./quick-agent-install.html">${esc(ui.homeAgentPathTitle)}</a></h2><p>${esc(ui.homeAgentPathBody)}</p></article><article class="doc-card"><h2><a href="./install.html">${esc(ui.homeManualPathTitle)}</a></h2><p>${esc(ui.homeManualPathBody)}</p></article><article class="doc-card"><h2><a href="./extension.html">${esc(ui.homeBrowserPathTitle)}</a></h2><p>${esc(ui.homeBrowserPathBody)}</p></article></div></section>

        <section class="home-section" id="outcomes"><div class="index-group-heading"><span class="eyebrow">04</span><h2>${esc(ui.homeOutcomesTitle)}</h2></div><ul class="home-checks"><li>${esc(ui.homeOutcome1)}</li><li>${esc(ui.homeOutcome2)}</li><li>${esc(ui.homeOutcome3)}</li></ul></section>

        <section class="home-section home-safety" id="safety"><div class="index-group-heading"><span class="eyebrow">05</span><h2>${esc(ui.homeSafetyTitle)}</h2><p>${esc(ui.homeSafetyBody)}</p></div></section>

        <section class="home-section" id="support"><div class="index-group-heading"><span class="eyebrow">06</span><h2>${esc(ui.homeSupportTitle)}</h2></div><div class="docs-grid home-three"><article class="doc-card"><h2><a href="./troubleshooting.html">${esc(docsBySlug.get("troubleshooting").title)}</a></h2><p>${esc(docsBySlug.get("troubleshooting").description)}</p></article><article class="doc-card"><h2><a href="./architecture.html">${esc(docsBySlug.get("architecture").title)}</a></h2><p>${esc(docsBySlug.get("architecture").description)}</p></article><article class="doc-card"><h2><a href="https://github.com/whshang/herdr-mcp/tree/main/docs/history">${esc(ui.homeHistory)}</a></h2><p>${esc(ui.historyNav)}</p></article></div></section>
      </div>
      <footer class="docs-footer"><a href="https://github.com/whshang/herdr-mcp">GitHub</a><a href="./privacy.html">${esc(docsBySlug.get("privacy").title)}</a></footer>
    </main>
    <aside class="toc" aria-label="${esc(ui.onThisPage)}"><div class="toc-inner"><h2>${esc(ui.onThisPage)}</h2><ol><li><a href="#agent-install">${esc(ui.homeAgentPathTitle)}</a></li><li><a href="#agent-does">${esc(ui.homeWillDoTitle)}</a></li><li><a href="#handoffs">${esc(ui.homeHandoffsTitle)}</a></li><li><a href="#paths">${esc(ui.homePathsTitle)}</a></li><li><a href="#safety">${esc(ui.homeSafetyTitle)}</a></li></ol></div></aside>
  </div>
  ${searchUi({ pre, locale, searchIndex })}
</body>
</html>`;
}

// Neutral /docs/ entry: no language chooser. Default locale is English, but a
// zh browser (without an explicit language pin) is routed to the Simplified
// Chinese docs so the neutral entry never dumps Chinese readers into English.
function docsRedirectShell() {
  const ui = UI[DEFAULT_LOCALE];
  const enHome = `${origin}/docs/en/`;
  const zhHome = `${origin}/docs/zh-CN/`;
  return `<!doctype html>
<html lang="${ui.htmlLang}">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width,initial-scale=1">
  <meta name="description" content="${esc(ui.indexLead)}">
  <meta name="color-scheme" content="light dark">
  <title>herdr-mcp ${esc(ui.docsNav)}</title>
  <link rel="canonical" href="${esc(enHome)}">
  <link rel="alternate" hreflang="en" href="${esc(enHome)}">
  <link rel="alternate" hreflang="zh-CN" href="${esc(zhHome)}">
  <link rel="alternate" hreflang="x-default" href="${esc(enHome)}">
  <meta http-equiv="refresh" content="0; url=./en/">
  <link rel="icon" type="image/png" href="../favicon.png">
  <link rel="stylesheet" href="../style.css">
</head>
<body class="docs-index-page">
  <main class="docs-index"><div class="redirect-note"><span class="eyebrow">${esc(ui.docsNav)}</span><h1>herdr-mcp ${esc(ui.docsNav)}</h1><p>${esc(ui.indexLead)}</p><p class="redirect-links"><a href="./en/">English →</a><a href="./zh-CN/">简体中文</a></p></div></main>
  <script>
    var langKey = "herdr-docs-lang";
    var wantsZh = !localStorage.getItem(langKey) && (navigator.languages || [navigator.language]).some(function (l) { return (l || "").toLowerCase().indexOf("zh") === 0; });
    location.replace(wantsZh ? "./zh-CN/" : "./en/");
  </script>
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
}

// Cross-language search: every item also carries the same document's title,
// headings and blurb from the other locale, so a query typed in either language
// finds the document from either side of the site.
for (const locale of LOCALES) {
  const { docsBySlug, rendered, searchIndex } = byLocale.get(locale);
  const other = LOCALES.find((lang) => lang !== locale);
  const otherData = byLocale.get(other);
  for (const item of searchIndex) {
    const slug = item.href.replace(/^\.\//, "").replace(/\.html$/, "");
    const od = otherData.docsBySlug.get(slug);
    const op = otherData.rendered.get(slug);
    if (od && op) {
      item.aliases = [od.title, od.description, ...op.toc.map((heading) => heading.title)].filter((text) => typeof text === "string" && text.length > 0);
    }
  }
  byLocale.set(locale, { ...byLocale.get(locale), searchIndex });

  const localeOut = join(outDir, "docs", locale);
  await mkdir(localeOut, { recursive: true });
  for (const slug of DOC_ORDER) {
    const doc = docsBySlug.get(slug);
    const page = rendered.get(slug);
    await writeFile(
      join(localeOut, `${slug}.html`),
      articleShell({ locale, doc, body: page.html, toc: page.toc, docsBySlug, searchIndex, version: runtimeVersion })
    );
  }
  await writeFile(join(localeOut, "index.html"), localeEntryShell({ locale, docsBySlug, searchIndex, version: runtimeVersion }));
}

await writeFile(join(outDir, "docs", "index.html"), docsRedirectShell());

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
  version: runtimeVersion,
  commit,
  docs: "./docs/",
  skill,
}, null, 2)}\n`);
await writeFile(join(outDir, ".nojekyll"), "");

console.log(JSON.stringify({ ok: true, output: "site-dist", locales: LOCALES.map((l) => `${l}*${DOC_ORDER.length}`), docs: DOC_ORDER.length, version: runtimeVersion, commit }));