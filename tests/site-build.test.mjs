import test from "node:test";
import assert from "node:assert/strict";
import { access, readFile, readdir, rm } from "node:fs/promises";
import { constants } from "node:fs";
import { spawnSync } from "node:child_process";
import { join } from "node:path";
import {
  DEFAULT_LOCALE,
  DOC_ORDER,
  LOCALES,
  LOCALE_NAMES,
  NAV_GROUPS,
  NAV_GROUP_LABELS,
  UI,
} from "../scripts/site-i18n.mjs";

const ROOT = new URL("../", import.meta.url).pathname;
const OUT = join(ROOT, "site-dist");
const ORIGIN = "https://whshang.github.io/herdr-mcp";

function section(html, start, end) {
  const from = html.indexOf(start);
  assert.notEqual(from, -1, `missing section start: ${start}`);
  const to = html.indexOf(end, from);
  assert.notEqual(to, -1, `missing section end: ${end}`);
  return html.slice(from, to + end.length);
}

function matches(text, regex) {
  return [...text.matchAll(regex)];
}

// First document the docs entries land on; short (sidebar-safe) titles.
const FIRST_SLUG = DOC_ORDER[0];
const EN_TITLE = "Architecture";
const ZH_TITLE = "架构";

test("documentation site build publishes every logical doc x 2 locales under locale-aware URLs", async () => {
  await rm(OUT, { recursive: true, force: true });
  const env = {
    ...process.env,
    GITHUB_SHA: "ambient-ci-sha",
    HERDR_SITE_COMMIT: "site-build-test",
  };
  const run = spawnSync(process.execPath, [join(ROOT, "scripts", "build-site.mjs")], {
    cwd: ROOT,
    encoding: "utf8",
    env,
  });
  assert.equal(run.status, 0, run.stderr || run.stdout);
  const result = JSON.parse(run.stdout.trim().split(/\r?\n/).at(-1));
  assert.equal(result.ok, true);
  assert.deepEqual(result.locales, LOCALES.map((locale) => `${locale}*${DOC_ORDER.length}`));
  assert.equal(result.docs, DOC_ORDER.length);

  for (const locale of LOCALES) {
    for (const slug of DOC_ORDER) {
      await access(join(OUT, "docs", locale, `${slug}.html`), constants.R_OK);
      await access(join(ROOT, "docs", "i18n", locale, `${slug}.md`), constants.R_OK);
    }
    await access(join(OUT, "docs", locale, "index.html"), constants.R_OK);
  }

  for (const rel of ["index.html", "style.css", "app.js", "zh-CN/index.html", "docs/index.html", "herdr-mcp-SKILL.md", "release.json", ".nojekyll"]) {
    await access(join(OUT, rel), constants.R_OK);
  }

  // Old flat /docs/<slug>.html landing pages must not exist anymore: the same
  // logical slug now lives under each locale dir.
  const files = await readdir(join(OUT, "docs"));
  const flat = files.filter((name) => DOC_ORDER.includes(name.replace(/\.html$/, "")));
  assert.deepEqual(flat, [], `flat single-language doc pages must not be published: ${flat.join(",")}`);
});

test("neutral /docs/ entry routes by browser language straight to the first document", async () => {
  assert.equal(DEFAULT_LOCALE, "en", "English must be the default locale");
  const entry = await readFile(join(OUT, "docs", "index.html"), "utf8");
  assert.match(entry, /<html lang="en">/);
  // No language chooser: no locale cards, no flattened doc nav, no per-locale search.
  assert.doesNotMatch(entry, /chooser|data-doc-slug|class="doc-card"/, "docs entry must not be a language chooser");
  assert.doesNotMatch(entry, /data-search-open|data-search-dialog/, "docs entry is a redirect, not a searchable page");
  const first = DOC_ORDER[0];
  // Default forwards to the first English document; zh browsers are routed to
  // the Chinese first document so the entry never dumps them into English.
  assert.match(entry, new RegExp(`<meta http-equiv="refresh" content="0; url=\\./en/${first}\\.html">`));
  assert.match(entry, /herdr-docs-lang/, "docs entry must carry the language-routing script");
  assert.match(entry, new RegExp(`location\\.replace\\(wantsZh \\? "\\./zh-CN/${first}\\.html" : "\\./en/${first}\\.html"\\)`));
  assert.match(entry, new RegExp(`href="\\./en/${first}\\.html"`), "fallback link to the English first document");
  assert.match(entry, new RegExp(`href="\\./zh-CN/${first}\\.html"`), "fallback link to the Chinese first document");
  assert.match(entry, new RegExp(`rel="canonical" href="${ORIGIN}/docs/en/${first}\\.html"`));
  assert.deepEqual(
    matches(entry, /rel="alternate" hreflang="([^"]+)"/g).map((m) => m[1]).filter((lang) => lang !== "x-default"),
    LOCALES,
    "docs entry must advertise one hreflang alternate per locale"
  );
  assert.match(entry, new RegExp(`rel="alternate" hreflang="x-default" href="${ORIGIN}/docs/${DEFAULT_LOCALE}/${first}\\.html"`));
  assert.match(entry, /href="\.\.\/style\.css"/);
});

test("each locale docs entry redirects straight to the first document page", async () => {
  for (const locale of LOCALES) {
    const ui = UI[locale];
    const first = DOC_ORDER[0];
    const entry = await readFile(join(OUT, "docs", locale, "index.html"), "utf8");
    assert.match(entry, new RegExp(`<html lang="${ui.htmlLang}">`));
    assert.match(entry, new RegExp(`rel="canonical" href="${ORIGIN}/docs/${locale}/${first}\\.html"`));
    for (const lang of LOCALES) {
      assert.match(entry, new RegExp(`rel="alternate" hreflang="${lang}" href="${ORIGIN}/docs/${lang}/${first}\\.html"`));
    }
    assert.match(entry, new RegExp(`rel="alternate" hreflang="x-default" href="${ORIGIN}/docs/${DEFAULT_LOCALE}/${first}\\.html"`));
    assert.match(entry, new RegExp(`<meta http-equiv="refresh" content="0; url=\\./${first}\\.html">`));
    assert.match(entry, new RegExp(`location\\.replace\\("\\./${first}\\.html"\\)`));
    // A bare redirect: no landing hero, no cards, no search surface, no agent intro.
    assert.doesNotMatch(entry, /class="doc-card"|data-nav-group|data-search-open|data-agent-intro/, `locale entry (${locale}) must be a bare redirect`);
  }
});

test("article pages carry same-slug language switching, per-locale search isolation, hreflang, TOC and prev/next", async () => {
  for (const locale of LOCALES) {
    const ui = UI[locale];
    for (const slug of DOC_ORDER) {
      const html = await readFile(join(OUT, "docs", locale, `${slug}.html`), "utf8");
      assert.match(html, new RegExp(`<html lang="${ui.htmlLang}">`));
      assert.match(html, /class="topbar has-drawer"/);
      assert.match(html, /data-nav-toggle/);

      // Sidebar: every document exactly once in order, only the active one marked.
      const sidebar = section(html, '<nav class="sidebar-nav"', "</nav>");
      const sidebarSlugs = matches(sidebar, /data-doc-slug="([^"]+)"/g).map((m) => m[1]);
      assert.deepEqual(sidebarSlugs, DOC_ORDER, `article sidebar (${locale}/${slug}) must contain every document exactly once`);
      assert.match(sidebar, new RegExp(`data-doc-slug="${slug}"[^>]*aria-current="page"`));
      assert.equal(matches(sidebar, /aria-current="page"/g).length, 1);
      let lastGroupOffset = -1;
      NAV_GROUP_LABELS[locale].forEach((label) => {
        const offset = sidebar.indexOf(`<h2>${label.replaceAll("&", "&amp;")}</h2>`);
        assert.ok(offset > lastGroupOffset, `sidebar group ${label} (${locale}/${slug}) must appear in configured order`);
        lastGroupOffset = offset;
      });

      // Same-slug language switch: exactly the other locale's same slug.
      const switcher = section(html, '<nav class="lang-switcher"', "</nav>");
      for (const lang of LOCALES) {
        const isCurrent = lang === locale;
        const currentAttr = isCurrent ? ' aria-current="true"' : "";
        assert.ok(
          switcher.includes(`<a href="../${lang}/${slug}.html" hreflang="${lang}" data-locale="${lang}"${currentAttr}>${LOCALE_NAMES[lang]}</a>`),
          `same-slug switcher maps ${locale}/${slug} -> ${lang}/${slug}`
        );
      }
      const other = LOCALES.find((lang) => lang !== locale);
      assert.equal(matches(switcher, new RegExp(`href="\\.\\./${other}/${slug}\\.html"`, "g")).length, 1, `exactly one switch to ${other} same slug (${locale}/${slug})`);
      assert.match(html, /class="brand" href="\.\.\/\.\.\/"/);

      // Canonical is self; alternates cover both locales; x-default -> default-locale same slug.
      assert.match(html, new RegExp(`rel="canonical" href="${ORIGIN}/docs/${locale}/${slug}\\.html"`));
      for (const lang of LOCALES) {
        assert.match(html, new RegExp(`rel="alternate" hreflang="${lang}" href="${ORIGIN}/docs/${lang}/${slug}\\.html"`));
      }
      assert.match(html, new RegExp(`rel="alternate" hreflang="x-default" href="${ORIGIN}/docs/${DEFAULT_LOCALE}/${slug}\\.html"`));

      // Translated UI labels on the article.
      assert.ok(html.includes(ui.onThisPage), `On-this-page label (${locale})`);
      assert.ok(html.includes(ui.searchLabel), `search label (${locale})`);
      assert.ok(html.includes(ui.openNav), `drawer open label (${locale})`);
      assert.ok(html.includes(ui.editSource), `edit-source footer label (${locale})`);

      // Server-rendered headings: unique ids, matching TOC, # heading-anchor links.
      const articleBody = section(html, '<article class="doc-body"', "</article>");
      const headingIds = matches(articleBody, /<h[23] id="([^"]+)"/g).map((m) => m[1]);
      assert.equal(new Set(headingIds).size, headingIds.length, `heading ids must be unique (${locale}/${slug})`);
      const toc = section(html, '<aside class="toc"', "</aside>");
      for (const id of headingIds) {
        assert.match(toc, new RegExp(`class="toc-depth-[23]"><a href="#${id.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}">`));
      }
      assert.match(html, /class="heading-anchor" href="#[^"]+"/);

      // Local markdown links are rewritten to same-locale html; no raw .md leaks.
      assert.doesNotMatch(html, /href="\.\/[^"#?]+\.md(?:#|"|\?)/, `raw local .md link (${locale}/${slug})`);
      assert.doesNotMatch(html, /href="(?![^"]*github\.com)(?![^"]*herdr-mcp-SKILL\.md)[^"]*\.md"/, `unrewritten markdown href (${locale}/${slug})`);
      for (const sl of DOC_ORDER) {
        assert.match(html, new RegExp(`href="\\./${sl}\\.html"`), `same-locale doc link for ${sl} (${locale}/${slug})`);
      }

      // Localized prev/next navigation within the locale.
      const pageIndex = DOC_ORDER.indexOf(slug);
      if (pageIndex > 0) {
        assert.match(html, new RegExp(`data-prev href="\\./${DOC_ORDER[pageIndex - 1]}\\.html"`));
        assert.ok(html.includes(`<span>${ui.previous}</span>`), `previous label (${locale}/${slug})`);
      }
      else assert.doesNotMatch(html, /data-prev/, "first document must not render prev");
      if (pageIndex < DOC_ORDER.length - 1) {
        assert.match(html, new RegExp(`data-next href="\\./${DOC_ORDER[pageIndex + 1]}\\.html"`));
        assert.ok(html.includes(`<span>${ui.next}</span>`), `next label (${locale}/${slug})`);
      }
      else assert.doesNotMatch(html, /data-next/, "last document must not render next");

      // Search index: this locale's documents, own-locale titles, plus
      // cross-language aliases so the same term finds docs from either side.
      const searchDataMatch = html.match(/<script id="search-index" type="application\/json">([\s\S]*?)<\/script>/);
      assert.ok(searchDataMatch, `search index must be embedded (${locale}/${slug})`);
      const searchData = JSON.parse(searchDataMatch[1]);
      assert.deepEqual(searchData.map((item) => item.href), DOC_ORDER.map((sl) => `./${sl}.html`));
      const zhTitles = searchData.some((item) => item.title === ZH_TITLE || item.title.includes("安装"));
      const enTitles = searchData.some((item) => item.title === EN_TITLE || item.title.includes("Install"));
      if (locale === "zh-CN") {
        assert.ok(zhTitles, "zh-CN search index must contain zh-CN titles");
        assert.ok(!enTitles, "zh-CN search index must not contain en titles as item titles");
      } else {
        assert.ok(enTitles, "en search index must contain en titles");
        assert.ok(!zhTitles, "en search index must not contain zh-CN titles as item titles");
      }
      assert.ok(searchData.every((item) => Array.isArray(item.headings)));
      // Every item exposes the other locale's same document as searchable aliases.
      assert.ok(
        searchData.every((item) => Array.isArray(item.aliases) && item.aliases.length > 0),
        `cross-language search aliases (${locale}/${slug})`
      );
      const firstEntry = searchData.find((item) => item.href === `./${FIRST_SLUG}.html`);
      if (locale === "en") {
        assert.ok((firstEntry.aliases || []).some((alias) => alias.includes(ZH_TITLE)), "en index aliases the Chinese title");
      } else {
        assert.ok((firstEntry.aliases || []).some((alias) => alias.toLowerCase().includes(EN_TITLE.toLowerCase())), "zh-CN index aliases the English title");
      }

      // Translated search UI blob consumed by app.js.
      const i18nMatch = html.match(/<script id="search-i18n" type="application\/json">([\s\S]*?)<\/script>/);
      assert.ok(i18nMatch, `search-i18n blob must be embedded (${locale}/${slug})`);
      const i18n = JSON.parse(i18nMatch[1]);
      assert.equal(i18n.hint, ui.searchHint);
      assert.equal(i18n.noResults, ui.searchNoResults);
      assert.equal(i18n.copy, ui.copyCode);
      assert.equal(i18n.copied, ui.copiedCode);

      // Code blocks carry a copy button — every <pre> in the doc body gets one
      // (the agent-intro prompt box below the article is exempt by design).
      assert.equal(
        matches(articleBody, /data-copy-code/g).length,
        matches(articleBody, /<pre/g).length,
        `every code block gets a copy button (${locale}/${slug})`
      );
      // Version badge in the topbar.
      assert.match(html, /class="version-badge"/, `version badge (${locale}/${slug})`);
      // "Let your agent introduce you" lives on the first document page only.
      if (slug === FIRST_SLUG) {
        assert.match(html, /data-agent-intro/, `agent-intro block on the first document (${locale})`);
        assert.match(html, /href="\.\.\/\.\.\/herdr-mcp-SKILL\.md"/, `skill link from the first document (${locale})`);
      } else {
        assert.doesNotMatch(html, /data-agent-intro/, `agent-intro must only render on the first document (${locale}/${slug})`);
      }
    }
  }

  // Sidebar titles stay per-locale; search aliases are cross-locale by design.
  const zn = await readFile(join(OUT, "docs", "zh-CN", `${FIRST_SLUG}.html`), "utf8");
  const en = await readFile(join(OUT, "docs", "en", `${FIRST_SLUG}.html`), "utf8");
  const znSidebar = section(zn, '<nav class="sidebar-nav"', "</nav>");
  const enSidebar = section(en, '<nav class="sidebar-nav"', "</nav>");
  assert.ok(znSidebar.includes(ZH_TITLE) && !znSidebar.includes(EN_TITLE), "zh-CN sidebar stays Chinese");
  assert.ok(enSidebar.includes(EN_TITLE) && !enSidebar.includes(ZH_TITLE), "en sidebar stays English");
});

test("shared runtime assets keep theme/drawer/search behavior and gain localized strings", async () => {
  const app = await readFile(join(OUT, "app.js"), "utf8");
  assert.match(app, /localStorage\.getItem\(themeKey\)/);
  assert.match(app, /prefers-color-scheme: dark/);
  assert.match(app, /event\.metaKey \|\| event\.ctrlKey/);
  assert.match(app, /event\.key === "Escape"/);
  assert.match(app, /aria-expanded/);
  assert.match(app, /showModal\(\)/);
  assert.match(app, /#search-i18n/, "app.js must read the per-page localized search UI blob");
  assert.match(app, /uiString\("hint"/);
  assert.match(app, /noResults/, "app.js must render the localized no-results string");
  assert.match(app, /data-copy-code/, "app.js must wire up code copy buttons");
  assert.match(app, /item\.aliases/, "app.js must match cross-language search aliases");
  assert.match(app, /herdr-docs-lang/, "app.js must pin the chosen language on switcher clicks");

  const css = await readFile(join(OUT, "style.css"), "utf8");
  assert.match(css, /--sidebar-w:\s*250px/);
  assert.match(css, /--toc-w:\s*220px/);
  assert.match(css, /--article-w:\s*760px/);
  assert.match(css, /@media \(max-width: 1023px\)/);
  assert.match(css, /@media \(max-width: 720px\)/);
  assert.match(css, /:root\[data-theme="dark"\]/);
  assert.match(css, /\.lang-switcher/, "language switcher styling");
  assert.doesNotMatch(css, /chooser/, "no leftover language-chooser styling");
  assert.match(css, /\.redirect-note/, "docs entry fallback styling");
  assert.match(css, /\.version-badge/, "version badge styling");
  assert.match(css, /\.code-copy/, "code copy button styling");
  assert.match(css, /\.agent-intro/, "agent-intro block styling");
  assert.match(css, /\.nav-group a \{[\s\S]*?white-space: nowrap/, "sidebar titles must not wrap");
});

test("release.json, skill artifact and design invariants are preserved", async () => {
  const release = JSON.parse(await readFile(join(OUT, "release.json"), "utf8"));
  const pkg = JSON.parse(await readFile(join(ROOT, "package.json"), "utf8"));
  assert.equal(release.version, pkg.version);
  assert.equal(release.commit, "site-build-test");
  assert.equal(release.docs, "./docs/");
  assert.equal(release.skill, "./herdr-mcp-SKILL.md");

  const skill = await readFile(join(OUT, "herdr-mcp-SKILL.md"), "utf8");
  assert.match(skill, /# herdr-mcp remote planner skill/);
  assert.match(skill, /dsh --profile headless/);

  const home = await readFile(join(OUT, "index.html"), "utf8");
  assert.match(home, /herdr-mcp/);
  assert.match(home, /<html lang="en">/);
  assert.match(home, /herdr-docs-lang/, "homepage must route zh browsers to the zh mirror");
  assert.match(home, /href="\.\/docs\/"/);
  assert.match(home, /href="\.\/docs\/en\/runtime-self-upgrade\.html"/);
  assert.match(home, /href="\.\/docs\/en\/capability-benchmark\.html"/);
  assert.match(home, /href="\.\/docs\/en\/cloudflare-edge-deployment\.html"/);
  // Homepage is bilingual: no hard-coded "Start locally", topbar switch + zh mirror.
  assert.doesNotMatch(home, /Start locally/);
  assert.match(home, /href="\.\/zh-CN\/index\.html"[^>]*hreflang="zh-CN"/);
  // Chinese homepage mirror mirrors content, paths and the reverse switch; its
  // documentation links all point at the Chinese docs (never the English ones).
  const homeZh = await readFile(join(OUT, "zh-CN", "index.html"), "utf8");
  assert.match(homeZh, /<html lang="zh-CN">/);
  assert.ok(matches(homeZh, /\.\.\/docs\/zh-CN\//g).length >= 5, "zh homepage docs links all point at zh-CN docs");
  assert.doesNotMatch(homeZh, /href="\.\.\/docs\/"/, "zh homepage must not link the bare (English) docs entry");
  assert.match(homeZh, /href="\.\.\/docs\/zh-CN\/runtime-self-upgrade\.html"/);
  assert.match(homeZh, /href="\.\.\/docs\/zh-CN\/capability-benchmark\.html"/);
  assert.match(homeZh, /href="\.\.\/" [^>]*hreflang="en"/);

  const generatedDocs = await readdir(join(OUT, "docs"));
  assert.equal(generatedDocs.some((name) => name.includes("_wip")), false);

  const readmeEn = await readFile(join(ROOT, "README.md"), "utf8");
  const readmeZh = await readFile(join(ROOT, "README.zh.md"), "utf8");
  const readmeJa = await readFile(join(ROOT, "README.ja.md"), "utf8");
  const edgeReadme = await readFile(join(ROOT, "edge", "cloudflare", "README.md"), "utf8");
  assert.match(readmeEn, /docs\/i18n\/en\//);
  assert.doesNotMatch(readmeEn, /docs\/i18n\/zh-CN\//);
  assert.match(readmeZh, /docs\/i18n\/zh-CN\//);
  assert.doesNotMatch(readmeZh, /docs\/i18n\/en\//);
  assert.match(readmeJa, /docs\/i18n\/en\//);
  assert.doesNotMatch(readmeJa, /docs\/i18n\/zh-CN\//);
  assert.match(readmeJa, /（英語）/);
  assert.match(edgeReadme, /\.\.\/\.\.\/docs\/i18n\/en\/cloudflare-edge-deployment\.md/);
  assert.match(edgeReadme, /\.\.\/\.\.\/docs\/i18n\/en\/runtime-self-upgrade\.md/);
  assert.doesNotMatch(edgeReadme, /docs\/i18n\/zh-CN\//);
});