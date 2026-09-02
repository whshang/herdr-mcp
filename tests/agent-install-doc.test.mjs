import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const read = (p) => readFileSync(path.join(ROOT, p), "utf8");

test("README gives the Agent one executable install sentence plus a short explanation", () => {
  const cases = [
    ["README.md", /Recommended: paste one sentence to your Agent/, /raw\.githubusercontent\.com\/whshang\/herdr-mcp\/main\/docs\/i18n\/en\/agent-install\.md/, /The Agent checks the machine/],
    ["README.zh.md", /推荐：给 Agent 一句话/, /raw\.githubusercontent\.com\/whshang\/herdr-mcp\/main\/docs\/i18n\/zh-CN\/agent-install\.md/, /Agent 会检查电脑环境/],
    ["README.ja.md", /推奨：Agent に一文だけ渡す/, /raw\.githubusercontent\.com\/whshang\/herdr-mcp\/main\/docs\/i18n\/en\/agent-install\.md/, /Agent は/],
  ];
  for (const [rel, heading, protocol, explanation] of cases) {
    const doc = read(rel);
    assert.match(doc, heading);
    assert.match(doc, protocol);
    assert.match(doc, /GitHub Release/i);
    assert.match(doc, explanation);
    assert.match(doc, /Cloudflare/);
    assert.match(doc, /ChatGPT/);
  }
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
    assert.match(doc, /provision-r2\.mjs/);
    assert.match(doc, /Workers R2 Storage/);
    assert.match(doc, /optional|可选/i);
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

test("maintainer install keeps deterministic Worker/bootstrap details while end-user entry stays short", () => {
  for (const rel of ["docs/i18n/en/agent-install.md", "docs/i18n/zh-CN/agent-install.md"]) {
    const doc = read(rel);
    assert.match(doc, /GitHub Releases/);
    assert.match(doc, /herdr-mcp install/);
    assert.doesNotMatch(doc, /## 2\.[^\n]*\n[\s\S]*?git clone https:\/\/github\.com\/whshang\/herdr-mcp/);
    assert.match(doc, /scripts\/cloudflare-worker-name\.mjs/);
    assert.match(doc, /WORKER_NAME/);
    assert.match(doc, /ACCOUNT_SUBDOMAIN/);
    assert.match(doc, /Cloudflare[^\n]*API/);
    assert.match(doc, /Chrome Web Store/);
    assert.match(doc, /STANDALONE/);
    assert.match(doc, /DEV/);
    assert.match(doc, /v0\.4\.2/);
    assert.match(doc, /extension/);
    assert.match(doc, /herdr-mcp native-host status/);
    assert.match(doc, /Native Messaging/);
    assert.match(doc, /HERDR_MCP_TOKEN/);
  }
  for (const rel of ["docs/i18n/en/install.md", "docs/i18n/zh-CN/install.md"]) {
    const doc = read(rel);
    assert.match(doc, /scripts\/cloudflare-worker-name\.mjs/);
    assert.match(doc, /WORKER_NAME/);
    assert.match(doc, /herdr-mcp install/);
  }
  for (const rel of ["README.md", "README.zh.md", "README.ja.md"]) {
    const doc = read(rel);
    assert.match(doc, /raw\.githubusercontent\.com\/whshang\/herdr-mcp\/main\/docs\/i18n\//);
    assert.match(doc, /Chrome Web Store/);
    assert.doesNotMatch(doc, /scripts\/cloudflare-worker-name\.mjs/);
    assert.doesNotMatch(doc, /## (?:Local runtime CLI|本机 runtime CLI)/);
  }
});

test("quick Agent protocols automate Herdr/runtime and preserve STORE STANDALONE DEV boundaries", () => {
  for (const rel of [
    "docs/i18n/en/agent-install.md",
    "docs/i18n/zh-CN/agent-install.md",
  ]) {
    const doc = read(rel);
    assert.match(doc, /herdr\.dev\/install\.(?:sh|ps1)/);
    assert.match(doc, /GitHub Releases?/);
    assert.match(doc, /HERDR_LINK_PROXY/);
    assert.match(doc, /Chrome Web Store/);
    assert.match(doc, /STANDALONE/);
    assert.match(doc, /DEV/);
    assert.match(doc, /v0\.4\.2/);
    assert.match(doc, /native-host use standalone/);
    assert.doesNotMatch(doc, /herdr-mcp-extension/);
    assert.doesNotMatch(doc, /cp -R extension|ln -s .*extension/);
    assert.match(doc, /source-development|源码开发/);
    assert.match(doc, /Do not build a proxy|不要自行搭代理/);
    assert.match(doc, /herdr-edge-device\.username\.workers\.dev\/mcp/);
    assert.match(doc, /herdr-mcp\.example\.com\/mcp/);
    assert.match(doc, /workers\.dev/);
    assert.match(doc, /custom domain|自定义域名/i);
    assert.doesNotMatch(doc, /second-mac-agent-prompt/);
  }
});

test("herdr-link resolves Node from PATH for fresh Apple Silicon installs", () => {
  const src = read("bin/herdr-link");
  assert.match(src, /HERDR_NODE_BIN/);
  assert.match(src, /command -v node/);
  assert.doesNotMatch(src, /^NODE_BIN="\/usr\/local\/bin\/node"$/m);
});

test("second-Mac GA UAT agent prompt enforces independent Worker, Link env override, and owner OAuth handoff", () => {
  for (const rel of [
  ]) {
    const doc = read(rel);
    assert.match(doc, /INTERNAL GA UAT|内部 GA UAT/);
    assert.match(doc, /not end-user install|非终端用户安装/);
    assert.match(doc, /begin execution immediately|立即阅读本 URL/);
    assert.match(doc, /herdr\.dev\/install\.sh/);
    assert.match(doc, /cloudflare-worker-name\.mjs/);
    assert.match(doc, /herdr-edge-prod/);
    assert.match(doc, /HERDR_EDGE_URL/);
    assert.match(doc, /HERDR_WORKSTATION_ID/);
    assert.match(doc, /HERDR_LINK_KEYCHAIN_SERVICE/);
    assert.match(doc, /PlistBuddy|patch plist/i);
    assert.match(doc, /workers_dev = true/);
    assert.match(doc, /TAG=v0\.4\.0/);
    assert.match(doc, /chrome:\/\/extensions/);
    assert.match(doc, /Edit Cloudflare Workers/);
    assert.match(doc, /CLOUDFLARE_API_TOKEN/);
    assert.match(doc, /Account Resources/);
    assert.match(doc, /Zone Resources/);
    assert.match(doc, /dash\.cloudflare\.com\/profile\/api-tokens/);
    assert.match(doc, /multi-device/i);
  }
  assert.doesNotMatch(read("docs/i18n/en/install.md"), /second-mac-ga-uat-agent-prompt/);
  assert.doesNotMatch(read("docs/i18n/zh-CN/install.md"), /second-mac-ga-uat-agent-prompt/);
});

test("browser privacy policy matches the Store-first extension data model", () => {
  for (const rel of ["docs/i18n/en/privacy.md", "docs/i18n/zh-CN/privacy.md"]) {
    const doc = read(rel);
    assert.match(doc, /chrome\.storage\.local/);
    assert.match(doc, /Native Messaging|nativeMessaging/);
    assert.match(doc, /OpenAI-compatible/);
    assert.match(doc, /Limited Use/);
    assert.match(doc, /remote executable code|远程可执行代码/);
    assert.match(doc, /credit|信用/);
  }
});
