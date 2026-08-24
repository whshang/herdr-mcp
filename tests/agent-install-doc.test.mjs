import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const read = (p) => readFileSync(path.join(ROOT, p), "utf8");

test("README exposes copyable local-Agent prompts that point to the authoritative raw guides", () => {
  const en = read("README.md");
  const zh = read("README.zh.md");
  const ja = read("README.ja.md");
  assert.match(en, /raw\.githubusercontent\.com\/whshang\/herdr-mcp\/main\/docs\/i18n\/en\/agent-install\.md/);
  assert.match(zh, /raw\.githubusercontent\.com\/whshang\/herdr-mcp\/main\/docs\/i18n\/zh-CN\/agent-install\.md/);
  assert.match(ja, /raw\.githubusercontent\.com\/whshang\/herdr-mcp\/main\/docs\/i18n\/en\/agent-install\.md/);
  assert.match(en, /Do not create a Custom Domain, DNS records or a Tunnel/);
  assert.match(zh, /不要创建 Custom Domain、DNS 记录或 Tunnel/);
});

test("Agent install guides own Cloudflare-token handoff and workers.dev-only bootstrap", () => {
  for (const rel of ["docs/i18n/en/agent-install.md", "docs/i18n/zh-CN/agent-install.md"]) {
    const doc = read(rel);
    assert.match(doc, /dash\.cloudflare\.com\/profile\/api-tokens/);
    assert.match(doc, /Edit Cloudflare Workers/);
    assert.match(doc, /Workers Scripts/);
    assert.match(doc, /CLOUDFLARE_API_TOKEN/);
    assert.match(doc, /CLOUDFLARE_ACCOUNT_ID/);
    assert.match(doc, /wrangler whoami/);
    assert.match(doc, /workers\/subdomain/);
    assert.match(doc, /PUT \/client\/v4\/accounts\/<ACCOUNT_ID>\/workers\/subdomain/);
    assert.match(doc, /wrangler deploy --config wrangler\.user\.toml/);
    assert.match(doc, /wrangler secret put LINK_SHARED_SECRET/);
    assert.match(doc, /workers_dev = true/);
    assert.match(doc, /routes = \[\]/);
  }
});

test("Agent install guide makes the bootstrap Token ephemeral and DNS-free", () => {
  const en = read("docs/i18n/en/agent-install.md");
  assert.match(en, /Never echo it/);
  assert.match(en, /shell history/);
  assert.match(en, /Unset `CLOUDFLARE_API_TOKEN`/);
  assert.match(en, /No Zone\/DNS mutation is required/);
});

test("Agent install uses deterministic Cloudflare-safe Worker names and reports the extension directory", () => {
  for (const rel of ["docs/i18n/en/agent-install.md", "docs/i18n/zh-CN/agent-install.md"]) {
    const doc = read(rel);
    assert.match(doc, /scripts\/cloudflare-worker-name\.mjs/);
    assert.match(doc, /WORKER_NAME/);
    assert.match(doc, /ACCOUNT_SUBDOMAIN/);
    assert.match(doc, /Cloudflare API/);
    assert.match(doc, /chrome:\/\/extensions/);
    assert.match(doc, /extension/);
    assert.match(doc, /herdr-extension-host install/);
    assert.match(doc, /herdr-extension-host status/);
    assert.match(doc, /Native Messaging/);
    assert.match(doc, /HERDR_MCP_TOKEN/);
  }
});

test("herdr-link resolves Node from PATH for fresh Apple Silicon installs", () => {
  const src = read("bin/herdr-link");
  assert.match(src, /HERDR_NODE_BIN/);
  assert.match(src, /command -v node/);
  assert.doesNotMatch(src, /^NODE_BIN="\/usr\/local\/bin\/node"$/m);
});
