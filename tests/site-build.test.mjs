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

// Distinct title markers proving search/nav isolation between locales.
const ZN_DISTINCT = "架构 — herdr 与 herdr-mcp";
const EN_DISTINCT = "Architecture — herdr and herdr-mcp";

test("documentation site build publishes 11 logical docs x 2 locales under locale-aware URLs", async () => {
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

  for (const rel of ["index.html", "style.css", "app.js", "docs/index.html", "herdr-mcp-SKILL.md", "release.json", ".nojekyll"]) {
    await access(join(OUT, rel), constants.R_OK);
  }

  // Old flat /docs/<slug>.html landing pages must not exist anymore: the same
  // logical slug now lives under each locale dir.
  const files = await readdir(join(OUT, "docs"));
  const flat = files.filter((name) => DOC_ORDER.includes(name.replace(/\.html$/, "")));
  assert.deepEqual(flat, [], `flat single-language doc pages must not be published: ${flat.join(",")}`);
});

test("neutral /docs/ entry is a bilingual chooser with no flattened doc nav", async () => {
  const chooser = await readFile(join(OUT, "docs", "index.html"), "utf8");
  assert.match(chooser, /<html lang="en">/);
  assert.doesNotMatch(chooser, /class="sidebar-nav"/, "chooser must not render a document sidebar");
  assert.doesNotMatch(chooser, /data-doc-slug|class="doc-card"/, "chooser must not flatten documents into one nav");
  assert.doesNotMatch(chooser, /data-search-open|data-search-dialog/, "chooser is language-neutral and has no per-locale search");
  assert.match(chooser, /href="\.\/zh-CN\/index\.html"[^>]*hreflang="zh-CN"/);
  assert.match(chooser, /href="\.\/en\/index\.html"[^>]*hreflang="en"/);
  assert.match(chooser, /rel="canonical" href="https:\/\/whshang\.github\.io\/herdr-mcp\/docs\/index\.html"/);
  assert.deepEqual(
    matches(chooser, /rel="alternate" hreflang="([^"]+)"/g).map((m) => m[1]).filter((lang) => lang !== "x-default"),
    LOCALES,
    "chooser must advertise one hreflang alternate per locale"
  );
  assert.match(chooser, /rel="alternate" hreflang="x-default" href="https:\/\/whshang\.github\.io\/herdr-mcp\/docs\/index\.html"/);
  // both languages visible on the neutral entry
  assert.match(chooser, /herdr-mcp 文档/);
  assert.match(chooser, /herdr-mcp documentation/);
  assert.equal(matches(chooser, /11 篇文档/g).length, 1, "zh-CN chooser card must not duplicate its document count");
  assert.equal(matches(chooser, /11 articles/g).length, 1, "en chooser card must not duplicate its document count");
  // chooser carries the shared runtime assets
  assert.match(chooser, /src="\.\.\/app\.js"/);
  assert.match(chooser, /href="\.\.\/style\.css"/);
  assert.match(chooser, /class="brand" href="\.\.\/"/);
});

test("each locale index is translated, grouped in currated order, and stays within its locale", async () => {
  for (const locale of LOCALES) {
    const ui = UI[locale];
    const index = await readFile(join(OUT, "docs", locale, "index.html"), "utf8");
    assert.match(index, new RegExp(`<html lang="${ui.htmlLang}">`));
    assert.match(index, /rel="canonical" href="https:\/\/whshang\.github\.io\/herdr-mcp\/docs\/[^/]+\/index\.html"/);
    for (const lang of LOCALES) {
      assert.match(index, new RegExp(`rel="alternate" hreflang="${lang}" href="https://whshang\\.github\\.io/herdr-mcp/docs/${lang}/index\\.html"`));
    }
    assert.match(index, /rel="alternate" hreflang="x-default" href="https:\/\/whshang\.github\.io\/herdr-mcp\/docs\/index\.html"/);

    // Translated hero and UI strings.
    assert.ok(index.includes(`<h1>${ui.indexTitle}</h1>`), `locale index hero title (${locale})`);
    assert.ok(index.includes(ui.indexLead), `locale index lead (${locale})`);
    assert.ok(index.includes(ui.indexCtaConnect), `locale index CTA connect (${locale})`);

    // Per-locale nav group labels in order.
    let lastGroupOffset = -1;
    NAV_GROUP_LABELS[locale].forEach((label, groupIndex) => {
      const offset = index.indexOf(`data-nav-group="${label.replaceAll("&", "&amp;")}"`);
      assert.ok(offset > lastGroupOffset, `group ${label} (${locale}) must appear in configured order`);
      lastGroupOffset = offset;
      assert.ok(NAV_GROUPS[groupIndex].slugs.length > 0, "group must own at least one document");
    });

    // Every document exactly once, in curated logical order, linked within the locale.
    const indexSlugs = matches(index, /<article class="doc-card" data-doc-slug="([^"]+)"/g).map((m) => m[1]);
    assert.deepEqual(indexSlugs, DOC_ORDER, `locale index (${locale}) must list every document exactly once in curated order`);
    for (const slug of DOC_ORDER) {
      assert.match(index, new RegExp(`<h2><a href="\\./${slug}\\.html">`), `index card link for ${slug} (${locale})`);
    }
    // Language switcher maps locale indexes.
    const switcher = section(index, '<nav class="lang-switcher"', "</nav>");
    for (const lang of LOCALES) {
      const currentAttr = lang === locale ? ' aria-current="true"' : "";
      assert.ok(
        switcher.includes(`<a href="../${lang}/index.html" hreflang="${lang}" data-locale="${lang}"${currentAttr}>${LOCALE_NAMES[lang]}</a>`),
        `locale index switcher must map ${locale} -> ${lang}`
      );
    }
    assert.match(index, /class="brand" href="\.\.\/\.\.\/"/);

    const indexWithoutSwitcher = index.replace(switcher, "");
    for (const other of LOCALES) {
      assert.doesNotMatch(indexWithoutSwitcher, new RegExp(`href="\\.\\./${other}/[^"]*\\.html"`), `locale index (${locale}) must not deep-link into ${other}`);
    }

    // Search is present and serves this locale only.
    assert.match(index, /data-search-open/);
    const searchDataMatch = index.match(/<script id="search-index" type="application\/json">([\s\S]*?)<\/script>/);
    assert.ok(searchDataMatch, "search index must be embedded in the locale index");
    const searchData = JSON.parse(searchDataMatch[1]);
    assert.deepEqual(searchData.map((item) => item.href), DOC_ORDER.map((slug) => `./${slug}.html`));
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
      assert.ok(html.includes(ui.docsIndex), `docs index footer label (${locale})`);

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
      assert.doesNotMatch(html, /href="(?![^"]*github\.com)[^"]*\.md"/, `unrewritten markdown href (${locale}/${slug})`);
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

      // Search index: exactly this locale's 11 documents, isolated from the other locale.
      const searchDataMatch = html.match(/<script id="search-index" type="application\/json">([\s\S]*?)<\/script>/);
      assert.ok(searchDataMatch, `search index must be embedded (${locale}/${slug})`);
      const searchData = JSON.parse(searchDataMatch[1]);
      assert.deepEqual(searchData.map((item) => item.href), DOC_ORDER.map((sl) => `./${sl}.html`));
      const zhTitles = searchData.some((item) => item.title.includes("herdr 与 herdr-mcp"));
      const enTitles = searchData.some((item) => item.title.includes("herdr and herdr-mcp"));
      if (locale === "zh-CN") {
        assert.ok(zhTitles, "zh-CN search index must contain zh-CN titles");
        assert.ok(!enTitles, "zh-CN search index must not contain en titles");
      } else {
        assert.ok(enTitles, "en search index must contain en titles");
        assert.ok(!zhTitles, "en search index must not contain zh-CN titles");
      }
      assert.ok(searchData.every((item) => Array.isArray(item.headings)));

      // Translated search UI blob consumed by app.js.
      const i18nMatch = html.match(/<script id="search-i18n" type="application\/json">([\s\S]*?)<\/script>/);
      assert.ok(i18nMatch, `search-i18n blob must be embedded (${locale}/${slug})`);
      const i18n = JSON.parse(i18nMatch[1]);
      assert.equal(i18n.hint, ui.searchHint);
      assert.equal(i18n.noResults, ui.searchNoResults);
    }
  }

  // Distinct locale titles prove crossed documents do not share nav/search.
  const zn = await readFile(join(OUT, "docs", "zh-CN", "architecture.html"), "utf8");
  const en = await readFile(join(OUT, "docs", "en", "architecture.html"), "utf8");
  assert.ok(zn.includes(ZN_DISTINCT) && !zn.includes(EN_DISTINCT));
  assert.ok(en.includes(EN_DISTINCT) && !en.includes(ZN_DISTINCT));
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

  const css = await readFile(join(OUT, "style.css"), "utf8");
  assert.match(css, /--sidebar-w:\s*250px/);
  assert.match(css, /--toc-w:\s*220px/);
  assert.match(css, /--article-w:\s*760px/);
  assert.match(css, /@media \(max-width: 1023px\)/);
  assert.match(css, /@media \(max-width: 720px\)/);
  assert.match(css, /:root\[data-theme="dark"\]/);
  assert.match(css, /\.lang-switcher/, "language switcher styling");
  assert.match(css, /\.chooser-grid/, "neutral chooser styling");
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
  assert.match(home, /href="\.\/docs\/"/);
  assert.match(home, /href="\.\/docs\/en\/runtime-self-upgrade\.html"/);
  assert.match(home, /href="\.\/docs\/en\/capability-benchmark\.html"/);
  assert.match(home, /href="\.\/docs\/en\/cloudflare-edge-deployment\.html"/);

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