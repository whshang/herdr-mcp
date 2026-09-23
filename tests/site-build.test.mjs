import test from "node:test";
import assert from "node:assert/strict";
import { access, readFile, readdir, rm } from "node:fs/promises";
import { constants } from "node:fs";
import { spawnSync } from "node:child_process";
import { join, basename } from "node:path";
import {
  DEFAULT_LOCALE,
  DOC_ORDER,
  LOCALES,
  LOCALE_NAMES,
  NAV_GROUPS,
  NAV_GROUP_LABELS,
  READING_ORDER,
  REDIRECTS,
  UI,
} from "../scripts/site-i18n.mjs";
import { checkLocale, formatReport } from "../scripts/i18n-doc-parity.mjs";

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

// First document the docs entries land on; short (sidebar-safe) titles per
// locale. JA_TITLE is the Japanese heading of the same page.
const FIRST_SLUG = DOC_ORDER[0];
const EN_TITLE = "Agent install";
const ZH_TITLE = "Agent 安装";
const JA_TITLE = "Agent インストール";

// Locale-specific markers for the same generated page, used to prove the
// search index and sidebars stay in their own language while aliases stay
// cross-language.
const TITLE_MARKERS = { en: /install/i, "zh-CN": /安装/, ja: /インストール/ };
const HOME_CONTINUATION = {
  en: /fresh manual conversation can simply say “continue”/,
  "zh-CN": /手动新开会话后可直接说“继续”/,
  ja: /手動で新規会話を開いたあと、「続けて」と言うだけで/,
};
// Same-slug page content proof for a behavior that must never drift when a
// page is localized: local continuation confirmation stays confirmation-first.
const CONTINUITY_CONFIRMATION = {
  en: /text-only match remains confirmation-required/,
  "zh-CN": /单纯文本匹配即使只剩一个候选也仍需要用户确认/,
  ja: /テキストのみの一致は候補が 1 つでもユーザーの確認が必要です/,
};

test("every locale source directory ships exactly the maintained document set", async () => {
  const expected = [...DOC_ORDER].sort();
  for (const locale of LOCALES) {
    const entries = await readdir(join(ROOT, "docs", "i18n", locale), { withFileTypes: true });
    const slugs = entries
      .filter((entry) => entry.isFile() && entry.name.endsWith(".md"))
      .map((entry) => entry.name.replace(/\.md$/, ""))
      .sort();
    assert.deepEqual(slugs, expected, `docs/i18n/${locale} must contain exactly the ${DOC_ORDER.length} logical docs`);
  }
});

test("site locale configuration keeps UI and navigation parity for every locale", () => {
  assert.deepEqual(LOCALES, ["en", "zh-CN", "ja"], "EN/ZH-CN/JA are the published locales");
  assert.equal(DEFAULT_LOCALE, "en", "English stays the default locale");
  assert.deepEqual(Object.keys(LOCALE_NAMES).sort(), [...LOCALES].sort());

  const enKeys = Object.keys(UI[DEFAULT_LOCALE]).sort();
  assert.ok(enKeys.length > 0, "the default locale defines the UI key contract");
  for (const locale of LOCALES) {
    assert.deepEqual(Object.keys(UI[locale]).sort(), enKeys, `UI key parity for ${locale}`);
    assert.equal(UI[locale].htmlLang, locale, `html lang for ${locale}`);
    assert.equal(NAV_GROUP_LABELS[locale].length, NAV_GROUPS.length, `one nav group label per group (${locale})`);
    assert.equal(LOCALE_NAMES[locale].length > 0, true);
  }

  // The Japanese locale must be real Japanese product copy, not English filler.
  const japanese = /[\u3040-\u30ff\u4e00-\u9fff]/;
  for (const key of ["docsNav", "onThisPage", "previous", "next", "searchHint", "indexTitle", "indexLead", "homeBrowserPathBody", "agentPrompt", "redirectFollow"]) {
    assert.match(UI.ja[key], japanese, `UI.ja.${key} must be Japanese`);
    assert.notEqual(UI.ja[key], UI.en[key], `UI.ja.${key} must not reuse the English string`);
  }
  assert.match(UI.ja.agentPrompt, /docs\/i18n\/ja\/agent-install\.md/, "the ja agent prompt points at the ja protocol");
  const englishNavLabels = new Set(NAV_GROUP_LABELS.en);
  for (const label of NAV_GROUP_LABELS.ja) {
    assert.match(label, japanese, `nav group label "${label}" must be Japanese`);
    assert.ok(!englishNavLabels.has(label), `nav group label "${label}" must not be the English label`);
  }
});

test("same-slug parity keeps internal doc links resolvable inside every locale", async () => {
  for (const locale of LOCALES) {
    const dir = join(ROOT, "docs", "i18n", locale);
    for (const slug of DOC_ORDER) {
      const source = await readFile(join(dir, `${slug}.md`), "utf8");
      for (const match of source.matchAll(/\]\(([^)\s]+\.md)(?:#[^)]*)?\)/g)) {
        const target = match[1];
        if (target.startsWith("../")) {
          await access(join(dir, target), constants.R_OK);
          continue;
        }
        const targetSlug = basename(target, ".md");
        assert.ok(DOC_ORDER.includes(targetSlug), `${locale}/${slug} links unknown logical doc ${target}`);
        await access(join(dir, `${targetSlug}.md`), constants.R_OK);
      }
    }
  }
});

test("Japanese docs mirror the canonical English structure, commands, links, identifiers and numbers", async () => {
  const report = await checkLocale({ root: ROOT, locale: "ja" });
  assert.equal(
    formatReport("ja", report),
    "",
    "every ja document must keep the English heading structure, fence sequence, shell commands, machine inline-code, numeric literals and link targets"
  );
});

test("documentation site build publishes every logical doc x 3 locales under locale-aware URLs", async () => {
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
    for (const [retired, target] of Object.entries(REDIRECTS)) {
      const redirect = await readFile(join(OUT, "docs", locale, `${retired}.html`), "utf8");
      assert.match(redirect, new RegExp(`<meta http-equiv="refresh" content="0; url=\\./${target}\\.html">`));
      assert.match(redirect, new RegExp(`rel="canonical" href="${ORIGIN}/docs/${locale}/${target}\\.html"`));
      assert.match(redirect, new RegExp(`href="\\./${target}\\.html"`));
      assert.ok(!DOC_ORDER.includes(retired), `${retired} must remain retired from first-class navigation`);
    }
  }

  for (const rel of ["index.html", "style.css", "app.js", "favicon.png", "zh-CN/index.html", "ja/index.html", "docs/index.html", "herdr-mcp-SKILL.md", "release.json", ".nojekyll"]) {
    await access(join(OUT, rel), constants.R_OK);
  }

  // Old flat /docs/<slug>.html landing pages must not exist anymore: the same
  // logical slug now lives under each locale dir.
  const files = await readdir(join(OUT, "docs"));
  const flat = files.filter((name) => DOC_ORDER.includes(name.replace(/\.html$/, "")));
  assert.deepEqual(flat, [], `flat single-language doc pages must not be published: ${flat.join(",")}`);
});

test("neutral /docs/ entry routes by browser language to real locale homepages", async () => {
  assert.equal(DEFAULT_LOCALE, "en", "English must be the default locale");
  assert.deepEqual(LOCALES, ["en", "zh-CN", "ja"], "the documented site is a three-locale surface");
  const entry = await readFile(join(OUT, "docs", "index.html"), "utf8");
  assert.match(entry, /<html lang="en">/);
  assert.doesNotMatch(entry, /data-search-open|data-search-dialog/, "neutral docs entry stays a lightweight locale router");
  assert.match(entry, /<meta http-equiv="refresh" content="0; url=\.\/en\/">/);
  assert.match(entry, /herdr-docs-lang/, "docs entry must carry the language-routing script");
  assert.match(entry, /if \(saved && homes\[saved\]\) return saved;/, "an explicit saved language choice must win over browser detection");
  assert.match(entry, /tag\.indexOf\("zh"\) === 0\) return "zh-CN"/, "docs entry must detect a zh browser");
  assert.match(entry, /tag\.indexOf\("ja"\) === 0\) return "ja"/, "docs entry must detect a ja browser");
  assert.match(entry, /var fallback = "en"/, "English stays the documented fallback");
  for (const locale of LOCALES) {
    assert.match(entry, new RegExp(`href="\\.\\/${locale}\\/"`), `fallback link to the ${locale} homepage`);
  }
  assert.match(entry, new RegExp(`rel="canonical" href="${ORIGIN}/docs/en/"`));
  assert.deepEqual(
    matches(entry, /rel="alternate" hreflang="([^"]+)"/g).map((m) => m[1]).filter((lang) => lang !== "x-default"),
    LOCALES,
    "docs entry must advertise one hreflang alternate per locale"
  );
  assert.match(entry, new RegExp(`rel="alternate" hreflang="x-default" href="${ORIGIN}/docs/${DEFAULT_LOCALE}/"`));
  assert.match(entry, /href="\.\.\/style\.css"/);
});

test("each locale docs entry is a real user-first homepage", async () => {
  for (const locale of LOCALES) {
    const ui = UI[locale];
    const entry = await readFile(join(OUT, "docs", locale, "index.html"), "utf8");
    assert.match(entry, new RegExp(`<html lang="${ui.htmlLang}">`));
    assert.match(entry, new RegExp(`rel="canonical" href="${ORIGIN}/docs/${locale}/"`));
    for (const lang of LOCALES) {
      assert.match(entry, new RegExp(`rel="alternate" hreflang="${lang}" href="${ORIGIN}/docs/${lang}/"`));
    }
    assert.match(entry, new RegExp(`rel="alternate" hreflang="x-default" href="${ORIGIN}/docs/${DEFAULT_LOCALE}/"`));
    assert.doesNotMatch(entry, /http-equiv="refresh"|location\.replace\(/, `locale homepage (${locale}) must not redirect to an article`);
    assert.match(entry, /class="docs-page docs-home-page"/);
    assert.match(entry, /data-agent-intro/);
    assert.match(entry, /data-search-open/);
    assert.match(entry, /data-search-dialog/);
    assert.match(entry, /data-copy-code/);
    assert.match(entry, /href="\.\/agent-install\.html"/);
    assert.match(entry, /href="\.\/install\.html"/);
    assert.match(entry, /href="\.\/overview\.html"/);
    assert.match(entry, /href="\.\/extension\.html"/);
    assert.match(entry, /Cloudflare/);
    assert.match(entry, /ChatGPT Connector/);
    assert.match(entry, /docs\/history/);
    assert.match(entry, new RegExp(ui.indexCtaConnect.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")));
    assert.match(entry, new RegExp(ui.homeHandoffsTitle.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")));
    const switcher = section(entry, '<nav class="lang-switcher"', "</nav>");
    for (const lang of LOCALES) {
      assert.match(switcher, new RegExp(`href="\\.\\./${lang}/index\\.html"`));
    }
    const sidebar = section(entry, '<nav class="sidebar-nav"', "</nav>");
    const sidebarSlugs = matches(sidebar, /data-doc-slug="([^"]+)"/g).map((m) => m[1]);
    assert.deepEqual(sidebarSlugs, DOC_ORDER, `homepage sidebar (${locale}) must preserve the document catalog order`);
    const homepageMain = section(entry, '<main class="article-column docs-home-column">', "</main>");
    assert.equal(matches(homepageMain, /class="button primary"/g).length, 1, `homepage (${locale}) must expose one primary CTA`);
    assert.match(homepageMain, /class="agent-prompt" tabindex="0"/, `homepage (${locale}) install prompt must be keyboard-focusable`);
    assert.match(homepageMain, /Chrome Web Store/, `homepage (${locale}) browser path must be Store-first`);
    assert.match(homepageMain, /durable continuity/, `homepage (${locale}) must explain no-ID continuity discovery`);
    assert.match(homepageMain, HOME_CONTINUATION[locale], `homepage (${locale}) must explain the no-ID continuation phrase`);
    assert.doesNotMatch(homepageMain, /\b(?:alpha|candidate|UAT|Runtime A\/B|worktree)\b|G\d+|GA scorecard/i, `homepage main content (${locale}) must keep release-engineering jargon out of the user path`);
  }
});

test("article pages carry same-slug language switching, per-locale search isolation, hreflang, TOC and prev/next", async () => {
  for (const locale of LOCALES) {
    const ui = UI[locale];
    for (const slug of DOC_ORDER) {
      const html = await readFile(join(OUT, "docs", locale, `${slug}.html`), "utf8");
      assert.match(html, new RegExp(`<html lang="${ui.htmlLang}">`));
      if (slug === "browser-continuity") {
        assert.match(html, /continuity\.search/);
        assert.match(html, /continuity_id/);
        assert.match(html, CONTINUITY_CONFIRMATION[locale], `no-ID continuity confirmation stays fail-closed (${locale})`);
      }
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
      const maintainerGroup = NAV_GROUPS.at(-1);
      assert.equal(maintainerGroup.secondary, true, "maintainer reference must stay secondary");
      assert.match(sidebar, new RegExp(`class="nav-group nav-secondary"><h2>${NAV_GROUP_LABELS[locale].at(-1).replaceAll("&", "&amp;")}</h2>`));

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
      const others = LOCALES.filter((lang) => lang !== locale);
      for (const other of others) {
        assert.equal(matches(switcher, new RegExp(`href="\\.\\./${other}/${slug}\\.html"`, "g")).length, 1, `exactly one switch to ${other} same slug (${locale}/${slug})`);
      }
      assert.equal(matches(switcher, /href="\.\.\/[^"]+\.html"/g).length, LOCALES.length, `switcher exposes exactly one link per locale (${locale}/${slug})`);
      assert.match(html, /class="brand" href="\.\.\/\.\.\/"/);
      assert.match(html, /rel="icon" type="image\/png" href="\.\.\/\.\.\/favicon\.png"/);

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

      // Localized prev/next navigation within the ordinary user reading flow.
      // Maintainer-only references remain discoverable in the sidebar/search,
      // but do not pull users into release-engineering material by default.
      const pageIndex = READING_ORDER.indexOf(slug);
      if (pageIndex < 0) {
        assert.doesNotMatch(html, /class="page-nav"/, `maintainer reference (${locale}/${slug}) must not join the ordinary reading chain`);
      }
      else if (pageIndex > 0) {
        assert.match(html, new RegExp(`data-prev href="\\./${READING_ORDER[pageIndex - 1]}\\.html"`));
        assert.ok(html.includes(`<span>${ui.previous}</span>`), `previous label (${locale}/${slug})`);
      }
      else assert.doesNotMatch(html, /data-prev/, "first document must not render prev");
      if (pageIndex >= 0 && pageIndex < READING_ORDER.length - 1) {
        assert.match(html, new RegExp(`data-next href="\\./${READING_ORDER[pageIndex + 1]}\\.html"`));
        assert.ok(html.includes(`<span>${ui.next}</span>`), `next label (${locale}/${slug})`);
      }
      else assert.doesNotMatch(html, /data-next/, "last or maintainer-only document must not render next");

      // Search index: this locale's documents, own-locale titles, plus
      // cross-language aliases so the same term finds docs from either side.
      const searchDataMatch = html.match(/<script id="search-index" type="application\/json">([\s\S]*?)<\/script>/);
      assert.ok(searchDataMatch, `search index must be embedded (${locale}/${slug})`);
      const searchData = JSON.parse(searchDataMatch[1]);
      assert.deepEqual(searchData.map((item) => item.href), DOC_ORDER.map((sl) => `./${sl}.html`));
      const ownMarker = TITLE_MARKERS[locale];
      assert.ok(searchData.some((item) => ownMarker.test(item.title)), `${locale} search index must contain ${locale} titles`);
      for (const other of LOCALES.filter((lang) => lang !== locale)) {
        assert.ok(
          searchData.every((item) => !TITLE_MARKERS[other].test(item.title)),
          `${locale} search index must not use ${other} titles as item titles`
        );
      }
      assert.ok(searchData.every((item) => Array.isArray(item.headings)));
      // Every item exposes every other locale's same document as searchable aliases.
      assert.ok(
        searchData.every((item) => Array.isArray(item.aliases) && item.aliases.length > 0),
        `cross-language search aliases (${locale}/${slug})`
      );
      const firstEntry = searchData.find((item) => item.href === `./${FIRST_SLUG}.html`);
      for (const other of LOCALES.filter((lang) => lang !== locale)) {
        assert.ok(
          (firstEntry.aliases || []).some((alias) => TITLE_MARKERS[other].test(alias)),
          `${locale} index aliases the ${other} title (${locale}/${slug})`
        );
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
  const ja = await readFile(join(OUT, "docs", "ja", `${FIRST_SLUG}.html`), "utf8");
  const znSidebar = section(zn, '<nav class="sidebar-nav"', "</nav>");
  const enSidebar = section(en, '<nav class="sidebar-nav"', "</nav>");
  const jaSidebar = section(ja, '<nav class="sidebar-nav"', "</nav>");
  assert.ok(znSidebar.includes(ZH_TITLE) && !znSidebar.includes(EN_TITLE), "zh-CN sidebar stays Chinese");
  assert.ok(enSidebar.includes(EN_TITLE) && !enSidebar.includes(ZH_TITLE), "en sidebar stays English");
  assert.ok(jaSidebar.includes(JA_TITLE) && !jaSidebar.includes(EN_TITLE) && !jaSidebar.includes(ZH_TITLE), "ja sidebar stays Japanese");
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
  for (const token of ["sidebar-w", "toc-w", "article-w"]) {
    assert.match(css, new RegExp(`--${token}:\\s*[^;]+;`), `layout token --${token} must stay defined`);
  }
  const responsiveBreakpoints = css.match(/@media \(max-width: \d+px\)/g) ?? [];
  assert.ok(responsiveBreakpoints.length >= 2, "docs layout must keep desktop/tablet and mobile responsive boundaries");
  assert.match(css, /\.topnav, \.version-badge \{ display: none; \}/, "mobile docs header hides redundant nav and version badge");
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
  const cargoManifest = await readFile(join(ROOT, "crates", "herdr-mcp", "Cargo.toml"), "utf8");
  const runtimeVersion = cargoManifest.match(/(?:^|\n)\[package\]\s*\n([\s\S]*?)(?=\n\[|$)/)?.[1]?.match(/^\s*version\s*=\s*"([^"]+)"\s*$/m)?.[1];
  assert.ok(runtimeVersion, "Rust runtime package version must be readable");
  assert.equal(release.version, runtimeVersion);
  assert.equal(pkg.version, runtimeVersion, "Node package metadata and Rust runtime package identity must stay aligned");
  const siteBuilder = await readFile(join(ROOT, "scripts", "build-site.mjs"), "utf8");
  assert.match(siteBuilder, /const runtimeVersion = parseCargoPackageVersion\(cargoManifest\);/, "site runtime version must remain sourced from Cargo.toml even when package versions are aligned");
  assert.equal(release.commit, "site-build-test");
  assert.equal(release.docs, "./docs/");
  assert.equal(release.skill, "./herdr-mcp-SKILL.md");

  const skill = await readFile(join(OUT, "herdr-mcp-SKILL.md"), "utf8");
  assert.match(skill, /# Herdr-MCP remote planner/);
  assert.match(skill, /Connected-tool calling contract/);
  assert.match(skill, /`params` as a \*\*JSON object string\*\*/);
  assert.match(skill, /`command` \*\*or\*\* `steps`, never both/);
  assert.match(skill, /workstation-control/);
  assert.match(skill, /development-orchestration/);
  assert.match(skill, /engineering-robustness/);
  assert.doesNotMatch(skill, /dsh --profile headless/);
  assert.doesNotMatch(skill, /grant_type=client_credentials/);

  const home = await readFile(join(OUT, "index.html"), "utf8");
  assert.match(home, /herdr-mcp/);
  const docsHome = await readFile(join(OUT, "docs", DEFAULT_LOCALE, "index.html"), "utf8");
  const zhDocsHome = await readFile(join(OUT, "docs", "zh-CN", "index.html"), "utf8");
  const escapedRuntimeVersion = runtimeVersion.replaceAll(".", "\\.");
  assert.match(docsHome, new RegExp(`class="version-badge"[^>]*aria-label="Source version: v${escapedRuntimeVersion}"[^>]*>Source version v${escapedRuntimeVersion}<`));
  assert.match(zhDocsHome, new RegExp(`class="version-badge"[^>]*aria-label="源码版本: v${escapedRuntimeVersion}"[^>]*>源码版本 v${escapedRuntimeVersion}<`));
  assert.doesNotMatch(docsHome, /Current version/, "main-built docs must not claim an unpublished source version is the current stable release");
  assert.match(home, /<html lang="en">/);
  assert.match(home, /rel="icon" type="image\/png" href="\.\/favicon\.png"/);
  assert.match(home, /herdr-docs-lang/, "homepage must route zh browsers to the zh mirror");
  assert.match(home, /href="\.\/docs\/en\/agent-install\.html"/, "homepage must make agent-assisted install the primary path");
  assert.match(home, /href="\.\/docs\/en\/install\.html"/, "homepage must keep manual install as a secondary path");
  assert.match(home, /href="\.\/docs\/en\/extension\.html"/, "homepage must expose the optional browser extension");
  assert.match(home, /href="\.\/docs\/en\/privacy\.html"/, "homepage must expose extension privacy one click away");
  assert.match(home, /href="\.\/docs\/en\/existing-worker-connect\.html"/, "homepage must expose the multi-device join path");
  assert.match(home, /Browser Control Center/);
  assert.match(home, /queued turns/i);
  assert.doesNotMatch(home, /href="\.\/docs\/en\/runtime-self-upgrade\.html"/, "maintainer runtime details stay out of the novice homepage");
  assert.doesNotMatch(home, /href="\.\/docs\/en\/capability-benchmark\.html"/, "capability benchmark stays out of the novice homepage");
  assert.match(home, /href="\.\/docs\/en\/cloudflare-edge-deployment\.html"/, "homepage must expose the supported Cloudflare setup path");
  // Homepage is bilingual: no hard-coded "Start locally", topbar switch + zh mirror.
  assert.doesNotMatch(home, /Start locally/);
  assert.match(home, /href="\.\/zh-CN\/index\.html"[^>]*hreflang="zh-CN"/);
  // Chinese homepage mirror mirrors content, paths and the reverse switch; its
  // documentation links all point at the Chinese docs (never the English ones).
  const homeZh = await readFile(join(OUT, "zh-CN", "index.html"), "utf8");
  assert.match(homeZh, /<html lang="zh-CN">/);
  assert.match(homeZh, /rel="icon" type="image\/png" href="\.\.\/favicon\.png"/);
  assert.ok(matches(homeZh, /\.\.\/docs\/zh-CN\//g).length >= 5, "zh homepage docs links all point at zh-CN docs");
  assert.doesNotMatch(homeZh, /href="\.\.\/docs\/"/, "zh homepage must not link the bare (English) docs entry");
  assert.match(homeZh, /href="\.\.\/docs\/zh-CN\/agent-install\.html"/);
  assert.match(homeZh, /href="\.\.\/docs\/zh-CN\/install\.html"/);
  assert.match(homeZh, /href="\.\.\/docs\/zh-CN\/privacy\.html"/);
  assert.match(homeZh, /href="\.\.\/docs\/zh-CN\/extension\.html"/);
  assert.match(homeZh, /href="\.\.\/docs\/zh-CN\/existing-worker-connect\.html"/);
  assert.match(homeZh, /Browser Control Center|浏览器控制中心/);
  assert.match(homeZh, /排队/);
  assert.doesNotMatch(homeZh, /href="\.\.\/docs\/zh-CN\/runtime-self-upgrade\.html"/);
  assert.doesNotMatch(homeZh, /href="\.\.\/docs\/zh-CN\/capability-benchmark\.html"/);
  assert.match(homeZh, /href="\.\.\/" [^>]*hreflang="en"/);

  // JA landing mirror: same structure, Japanese copy, JA docs links and a full
  // three-locale switcher; the EN landing detects a ja browser too.
  const homeJa = await readFile(join(OUT, "ja", "index.html"), "utf8");
  assert.match(homeJa, /<html lang="ja">/);
  assert.match(homeJa, /rel="icon" type="image\/png" href="\.\.\/favicon\.png"/);
  assert.ok(matches(homeJa, /\.\.\/docs\/ja\//g).length >= 5, "ja homepage docs links all point at ja docs");
  assert.doesNotMatch(homeJa, /href="\.\.\/docs\/"/, "ja homepage must not link the bare (English) docs entry");
  assert.match(homeJa, /href="\.\.\/docs\/ja\/agent-install\.html"/);
  assert.match(homeJa, /href="\.\.\/docs\/ja\/install\.html"/);
  assert.match(homeJa, /href="\.\.\/docs\/ja\/privacy\.html"/);
  assert.match(homeJa, /href="\.\.\/docs\/ja\/troubleshooting\.html"/);
  assert.match(homeJa, /main\/docs\/i18n\/ja\/agent-install\.md/, "ja homepage install prompt points at the ja protocol");
  assert.match(homeJa, /Browser Control Center/);
  assert.doesNotMatch(homeJa, /href="\.\.\/docs\/ja\/runtime-self-upgrade\.html"/);
  assert.doesNotMatch(homeJa, /href="\.\.\/docs\/ja\/capability-benchmark\.html"/);
  for (const locale of LOCALES) {
    const landing = locale === "en" ? home : locale === "zh-CN" ? homeZh : homeJa;
    assert.match(landing, new RegExp(`hreflang="${locale}"`), `landing (${locale}) advertises its own hreflang`);
    assert.match(
      landing,
      new RegExp(`data-locale="${locale}"`),
      `landing (${locale}) offers a ${locale} switcher entry`
    );
  }
  assert.match(home, /tag\.indexOf\("ja"\) === 0\) \{ location\.replace\("\.\/ja\/"\); break; \}/, "EN landing routes a ja browser to the ja mirror");
  assert.match(home, /saved === "ja"\) location\.replace\("\.\/ja\/"\)/, "EN landing honors an explicit saved ja choice");

  for (const locale of LOCALES) {
    const control = await readFile(join(OUT, "docs", locale, "browser-control-center.html"), "utf8");
    assert.match(control, /Chrome Side Panel/);
    assert.match(control, /Current page|当前页面/, "Control Center docs must expose active-tab page context");
    assert.match(control, /no drawer|没有抽屉/, "Control Center docs must keep the HUD as a compact non-duplicated surface");
    assert.match(control, /Manual handoff|手动接力/, "Control Center docs must explain that manual handoff belongs to the HUD conversation surface");
    assert.match(control, /Prompt Agent|提示 Agent/);
    assert.match(control, /Steer Session|调整会话/);
    assert.match(control, /trusted|可信|Native Messaging/, "Control Center docs must describe the trusted local action route");
    assert.match(control, /session_not_resolved/, "Control Center docs must distinguish unresolved provider steer from Prompt");
    assert.match(control, /Run command|运行命令/, "Control Center docs must describe direct fenced terminal command execution");
    assert.match(control, /Herdr API[\s\S]{0,160}(Preview only|preview-only|仅预览)/, "arbitrary Herdr API must remain preview-only");
    assert.match(control, /reload loop|刷新死循环/, "Control Center docs must keep bounded reload-loop protection in the reliability contract");
    assert.match(control, /Pinned Target|固定目标|pinned target/, "Control Center docs must document the explicit local target");
  }

  const generatedDocs = await readdir(join(OUT, "docs"));
  assert.equal(generatedDocs.some((name) => name.includes("_wip")), false);

  const readmeZh = await readFile(join(ROOT, "README.md"), "utf8");
  const readmeEn = await readFile(join(ROOT, "README.en.md"), "utf8");
  const readmeJa = await readFile(join(ROOT, "README.ja.md"), "utf8");
  const edgeReadme = await readFile(join(ROOT, "edge", "cloudflare", "README.md"), "utf8");
  assert.match(readmeEn, /docs\/i18n\/en\//);
  assert.doesNotMatch(readmeEn, /docs\/i18n\/zh-CN\//);
  assert.doesNotMatch(readmeEn, /docs\/i18n\/ja\//);
  assert.match(readmeZh, /docs\/i18n\/zh-CN\//);
  assert.doesNotMatch(readmeZh, /docs\/i18n\/en\//);
  assert.match(readmeJa, /docs\/i18n\/ja\//, "the Japanese README points at the Japanese maintained docs");
  assert.doesNotMatch(readmeJa, /docs\/i18n\/en\//, "the Japanese README must not fall back to English doc links");
  assert.doesNotMatch(readmeJa, /docs\/i18n\/zh-CN\//);
  assert.match(readmeJa, /\[English\]\(README\.en\.md\)/);
  assert.match(readmeJa, /\[简体中文\]\(README\.md\)/);
  assert.match(readmeZh, /\[English\]\(README\.en\.md\)/);
  assert.match(readmeEn, /\[简体中文\]\(README\.md\)/);
  assert.match(edgeReadme, /\.\.\/\.\.\/docs\/i18n\/en\/cloudflare-edge-deployment\.md/);
  assert.match(edgeReadme, /\.\.\/\.\.\/docs\/i18n\/en\/runtime-self-upgrade\.md/);
  assert.doesNotMatch(edgeReadme, /docs\/i18n\/zh-CN\//);
});
