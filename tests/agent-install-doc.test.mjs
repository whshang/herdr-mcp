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
  assert.match(en, /raw\.githubusercontent\.com\/whshang\/herdr-mcp\/main\/docs\/i18n\/en\/quick-agent-install\.md/);
  assert.match(zh, /raw\.githubusercontent\.com\/whshang\/herdr-mcp\/main\/docs\/i18n\/zh-CN\/quick-agent-install\.md/);
  assert.match(ja, /raw\.githubusercontent\.com\/whshang\/herdr-mcp\/main\/docs\/i18n\/en\/agent-install\.md/);
  assert.match(en, /quick-agent-install\.md/);
  assert.match(zh, /quick-agent-install\.md/);
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
    assert.match(doc, /GitHub Releases/);
    assert.match(doc, /herdr-mcp install/);
    assert.doesNotMatch(doc, /## 2\.[^\n]*\n[\s\S]*?git clone https:\/\/github\.com\/whshang\/herdr-mcp/);
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
  for (const rel of ["README.md", "README.zh.md", "docs/i18n/en/install.md", "docs/i18n/zh-CN/install.md"]) {
    const doc = read(rel);
    assert.match(doc, /scripts\/cloudflare-worker-name\.mjs/);
    assert.match(doc, /WORKER_NAME/);
    assert.match(doc, /herdr-mcp install/);
  }
});

test("quick agent install guides expose one-liner, proxy, custom domain, and extension path", () => {
  for (const rel of [
    "docs/i18n/en/quick-agent-install.md",
    "docs/i18n/zh-CN/quick-agent-install.md",
  ]) {
    const doc = read(rel);
    assert.match(doc, /quick-agent-install\.md/);
    assert.match(doc, /HERDR_LINK_PROXY/);
    assert.match(doc, /herdr-mcp-extension/);
    assert.match(doc, /herdr-edge-device\.username\.workers\.dev\/mcp/);
    assert.match(doc, /herdr-mcp\.example\.com\/mcp/);
    assert.match(doc, /workers\.dev/);
    assert.match(doc, /Custom Domain|自定义域名/);
    assert.doesNotMatch(doc, /second-mac-agent-prompt/);
  }
});

test("herdr-link resolves Node from PATH for fresh Apple Silicon installs", () => {
  const src = read("bin/herdr-link");
  assert.match(src, /HERDR_NODE_BIN/);
  assert.match(src, /command -v node/);
  assert.doesNotMatch(src, /^NODE_BIN="\/usr\/local\/bin\/node"$/m);
});

test("second-Mac agent prompt enforces independent Worker, Link env override, and owner OAuth handoff", () => {
  for (const rel of [
    "docs/i18n/en/second-mac-agent-prompt.md",
    "docs/i18n/zh-CN/second-mac-agent-prompt.md",
  ]) {
    const doc = read(rel);
    assert.match(doc, /herdr\.dev\/install\.sh/);
    assert.match(doc, /cloudflare-worker-name\.mjs/);
    assert.match(doc, /herdr-edge-prod/);
    assert.match(doc, /HERDR_EDGE_URL/);
    assert.match(doc, /HERDR_WORKSTATION_ID/);
    assert.match(doc, /HERDR_LINK_KEYCHAIN_SERVICE/);
    assert.match(doc, /PlistBuddy/);
    assert.match(doc, /workers_dev = true/);
    assert.match(doc, /v0\.4\.0-alpha\.16/);
    assert.match(doc, /chrome:\/\/extensions/);
    assert.match(doc, /multi-device/i);
  }
  const zhUat = read("docs/i18n/zh-CN/clean-machine-uat.md");
  const enUat = read("docs/i18n/en/clean-machine-uat.md");
  assert.match(zhUat, /second-mac-agent-prompt\.md/);
  assert.match(enUat, /second-mac-agent-prompt\.md/);
});
