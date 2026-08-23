import test from "node:test";
import assert from "node:assert/strict";
import { access, readFile, readdir, rm } from "node:fs/promises";
import { constants } from "node:fs";
import { spawnSync } from "node:child_process";
import { join } from "node:path";

const ROOT = new URL("../", import.meta.url).pathname;
const OUT = join(ROOT, "site-dist");
const NAV_GROUPS = [
  ["Start here", ["architecture", "chatgpt-connector"]],
  ["ChatGPT & Web", ["worker-fallbacks"]],
  ["Operations & Deployment", ["cloudflare-edge-deployment", "cloudflare-edge-token", "runtime-self-upgrade", "automation"]],
  ["Browser Extension", ["extension", "extension-wake", "extension-bridge"]],
  ["Reference & Development", ["capability-benchmark"]],
];
const NAV_ORDER = NAV_GROUPS.flatMap(([, slugs]) => slugs);

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

test("documentation site build publishes a grouped, navigable static docs shell", async () => {
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
  assert.equal(result.docs, NAV_ORDER.length);

  for (const rel of [
    "index.html",
    "style.css",
    "app.js",
    "docs/index.html",
    "docs/architecture.html",
    "docs/chatgpt-connector.html",
    "docs/cloudflare-edge-token.html",
    "docs/automation.html",
    "docs/runtime-self-upgrade.html",
    "docs/worker-fallbacks.html",
    "herdr-mcp-SKILL.md",
    "release.json",
    ".nojekyll",
  ]) {
    await access(join(OUT, rel), constants.R_OK);
  }

  const docsIndex = await readFile(join(OUT, "docs", "index.html"), "utf8");
  assert.match(docsIndex, /Remote planning, local execution\./);
  assert.match(docsIndex, /remote control plane between ChatGPT or another web planner and a local Herdr workstation/);
  assert.match(docsIndex, /href="\.\/chatgpt-connector\.html"[^>]*>Connect ChatGPT</);
  assert.match(docsIndex, /href="\.\/architecture\.html"[^>]*>Architecture</);
  assert.match(docsIndex, /href="\.\/cloudflare-edge-deployment\.html"[^>]*>Deploy the Edge</);
  assert.match(docsIndex, /data-search-open/);
  assert.match(docsIndex, /data-theme-toggle/);
  assert.doesNotMatch(docsIndex, /data-nav-toggle/, "docs index has no drawer, so it must not render a dead nav toggle");
  assert.match(docsIndex, /id="search-index" type="application\/json"/);
  assert.match(docsIndex, /src="\.\.\/app\.js"/);

  let lastGroupOffset = -1;
  for (const [label] of NAV_GROUPS) {
    const offset = docsIndex.indexOf(`data-nav-group="${label.replaceAll("&", "&amp;")}"`);
    assert.ok(offset > lastGroupOffset, `group ${label} must appear in configured order`);
    lastGroupOffset = offset;
  }
  const indexSlugs = matches(docsIndex, /<article class="doc-card" data-doc-slug="([^"]+)"/g).map((match) => match[1]);
  assert.deepEqual(indexSlugs, NAV_ORDER, "docs index must contain every document exactly once in curated order");

  const architecture = await readFile(join(OUT, "docs", "architecture.html"), "utf8");
  assert.match(architecture, /class="topbar has-drawer"/);
  assert.match(architecture, /data-nav-toggle/);
  const sidebar = section(architecture, '<nav class="sidebar-nav"', "</nav>");
  const sidebarSlugs = matches(sidebar, /data-doc-slug="([^"]+)"/g).map((match) => match[1]);
  assert.deepEqual(sidebarSlugs, NAV_ORDER, "article sidebar must contain every document exactly once");
  assert.match(sidebar, /data-doc-slug="architecture"[^>]*aria-current="page"/);
  assert.equal(matches(sidebar, /aria-current="page"/g).length, 1);

  let lastSidebarGroup = -1;
  for (const [label] of NAV_GROUPS) {
    const offset = sidebar.indexOf(`<h2>${label.replaceAll("&", "&amp;")}</h2>`);
    assert.ok(offset > lastSidebarGroup, `sidebar group ${label} must appear in configured order`);
    lastSidebarGroup = offset;
  }

  assert.match(architecture, /<aside class="toc" aria-label="On this page">/);
  const articleBody = section(architecture, '<article class="doc-body"', "</article>");
  const headingIds = matches(articleBody, /<h[23] id="([^"]+)"/g).map((match) => match[1]);
  assert.ok(headingIds.length >= 2, "article must receive server-generated h2/h3 IDs");
  assert.equal(new Set(headingIds).size, headingIds.length, "heading IDs must be unique");
  for (const id of headingIds) {
    assert.match(architecture, new RegExp(`class="toc-depth-[23]"><a href="#${id.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}">`));
  }
  assert.match(architecture, /class="heading-anchor" href="#[^"]+"/);
  assert.match(architecture, /href="\.\/capability-benchmark\.html"/);
  assert.doesNotMatch(architecture, /href="\.\/[^"#?]+\.md(?:#|"|\?)/, "local markdown links must be rewritten to built html");

  assert.doesNotMatch(architecture, /data-prev/);
  assert.match(architecture, /data-next href="\.\/chatgpt-connector\.html"/);
  const chatgpt = await readFile(join(OUT, "docs", "chatgpt-connector.html"), "utf8");
  assert.match(chatgpt, /data-prev href="\.\/architecture\.html"/);
  assert.match(chatgpt, /data-next href="\.\/worker-fallbacks\.html"/);
  const lastPage = await readFile(join(OUT, "docs", "capability-benchmark.html"), "utf8");
  assert.match(lastPage, /data-prev href="\.\/extension-bridge\.html"/);
  assert.doesNotMatch(lastPage, /data-next/);

  const searchDataMatch = architecture.match(/<script id="search-index" type="application\/json">([\s\S]*?)<\/script>/);
  assert.ok(searchDataMatch, "search index must be embedded in article pages");
  const searchData = JSON.parse(searchDataMatch[1]);
  assert.deepEqual(searchData.map((item) => item.href), NAV_ORDER.map((slug) => `./${slug}.html`));
  assert.ok(searchData.every((item) => Array.isArray(item.headings)));

  const app = await readFile(join(OUT, "app.js"), "utf8");
  assert.match(app, /localStorage\.getItem\(themeKey\)/);
  assert.match(app, /prefers-color-scheme: dark/);
  assert.match(app, /event\.metaKey \|\| event\.ctrlKey/);
  assert.match(app, /event\.key === "Escape"/);
  assert.match(app, /aria-expanded/);
  assert.match(app, /showModal\(\)/);

  const css = await readFile(join(OUT, "style.css"), "utf8");
  assert.match(css, /--sidebar-w:\s*250px/);
  assert.match(css, /--toc-w:\s*220px/);
  assert.match(css, /--article-w:\s*760px/);
  assert.match(css, /@media \(max-width: 1023px\)/);
  assert.match(css, /@media \(max-width: 720px\)/);
  assert.match(css, /:root\[data-theme="dark"\]/);

  const release = JSON.parse(await readFile(join(OUT, "release.json"), "utf8"));
  const pkg = JSON.parse(await readFile(join(ROOT, "package.json"), "utf8"));
  assert.equal(release.version, pkg.version);
  assert.equal(release.commit, "site-build-test");
  assert.equal(release.docs, "./docs/");
  assert.equal(release.skill, "./herdr-mcp-SKILL.md");

  const generatedDocs = await readdir(join(OUT, "docs"));
  assert.equal(generatedDocs.some((name) => name.includes("_wip")), false);
  const skill = await readFile(join(OUT, "herdr-mcp-SKILL.md"), "utf8");
  assert.match(skill, /# herdr-mcp remote planner skill/);
  assert.match(skill, /dsh --profile headless/);
});
