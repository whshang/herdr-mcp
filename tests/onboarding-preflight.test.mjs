import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const read = (p) => readFileSync(path.join(ROOT, p), "utf8");

const AGENT_INSTALL = [
  "docs/i18n/en/agent-install.md",
  "docs/i18n/zh-CN/agent-install.md",
  "docs/i18n/ja/agent-install.md",
];
const TROUBLESHOOTING = [
  "docs/i18n/en/troubleshooting.md",
  "docs/i18n/zh-CN/troubleshooting.md",
  "docs/i18n/ja/troubleshooting.md",
];

test("onboarding resolves first-vs-existing fleet intent before any Cloudflare deploy", () => {
  for (const rel of AGENT_INSTALL) {
    const doc = read(rel);
    // One explicit question gates the deploy path.
    assert.match(doc, /existing Herdr Worker|加入已有 Herdr Worker|既存の Herdr Worker に参加/);
    // Pairing evidence routes to the existing-fleet flow.
    assert.match(doc, /pairing address|pairing アドレス/);
    assert.match(doc, /worker pair/);
    assert.match(doc, /worker connect/);
    // A fresh machine must never probe fleet existence by running pair locally.
    assert.match(doc, /Never run `herdr-mcp worker pair` on the computer currently being installed|绝不能[^\n]*当前正在安装[^\n]*`herdr-mcp worker pair`|現在インストール中[^\n]*`herdr-mcp worker pair`[^\n]*実行してはいけません/);
    assert.match(doc, /already enrolled|already-enrolled|已经登记|已登记|enrollment 済み/);
    // Never deploy a second Worker/R2/Connector or evade ownership with a random suffix.
    assert.match(doc, /second Worker|R2 bucket|R2 桶|二つ目の R2 バケット/);
    assert.match(doc, /Never fall back[^\n]*random-suffixed Worker|禁止[^\n]*fallback[^\n]*随机后缀[^\n]*Worker|ランダムサフィックス付きの Worker/);
    assert.match(doc, /existing-worker-connect\.md/);
    // Pre-deploy existing-Worker detection via the Cloudflare API.
    assert.match(doc, /workers\/scripts/);
  }
  // The dedicated multi-device contract already forbids re-deployment.
  const fleet = read("docs/i18n/en/existing-worker-connect.md");
  assert.match(fleet, /do not deploy another Worker/);
  assert.match(fleet, /no owner\/member hierarchy/);
});

test("PATH preflight separates the installed binary from the interactive-shell PATH and self-heals", () => {
  for (const rel of AGENT_INSTALL) {
    const doc = read(rel);
    assert.match(doc, /installed_but_not_on_shell_path/);
    assert.match(doc, /second PATH owner|第二个 PATH owner|二つ目の PATH owner/);
    assert.doesNotMatch(doc, /grep -Fqx|zsh -ic/);
  }
  for (const rel of TROUBLESHOOTING) {
    const triage = read(rel);
    assert.match(triage, /installed_but_not_on_shell_path/);
    assert.match(triage, /zsh -ic 'command -v herdr-mcp'/);
    assert.match(triage, /zsh -lc 'command -v herdr-mcp'/);
    assert.match(triage, /export PATH="\$HOME\/\.local\/bin:\$PATH"/);
    assert.match(triage, /grep -Fqx[^\n]*\.zprofile/);
  }
});

test("macOS TCC/FDA readiness is verified before background setup, not after install", () => {
  for (const rel of AGENT_INSTALL) {
    const doc = read(rel);
    assert.match(doc, /herdr-mcp permissions verify/);
    assert.match(doc, /herdr-mcp permissions status/);
    assert.match(doc, /needs_setup/);
    assert.match(doc, /Full Disk Access|完全磁盘访问/);
    assert.match(doc, /before Cloudflare work|Cloudflare 工作(?:之前|前)|Cloudflare の作業の前/);
  }
});

test("first Worker handoff stays complete for a non-developer Mac user", () => {
  for (const rel of AGENT_INSTALL) {
    const doc = read(rel);
    assert.match(doc, /Workers Free/);
    assert.match(doc, /Google sign-in|Google 登录|Google サインイン/);
    assert.match(doc, /Developer mode/);
    assert.match(doc, /Plugins → Browse plugins|插件 → 浏览插件/);
    assert.match(doc, /`herdr`/);
    assert.match(doc, /workers\.dev\/mcp/);
    assert.match(doc, /ChatGPT Project|ChatGPT の Project/);
    assert.match(doc, /`\+` button|`\+` 加号|`\+` ボタン/);
    assert.match(doc, /first message|第一条消息|最初のメッセージ/);
  }
});

test("Cloudflare token preflight distinguishes a valid token from a missing permission", () => {
  for (const rel of AGENT_INSTALL) {
    const doc = read(rel);
    assert.match(doc, /Account Settings → Read/);
    assert.match(doc, /Workers Scripts → Write\/Edit/);
    // 403 diagnosis maps the failing call to the exact permission.
    assert.match(doc, /workers\/subdomain.*403|403.*workers\/subdomain/);
    assert.match(doc, /do not inflate|不要无根据扩大权限|Token のスコープを広げない/);
  }
  const tokenDoc = read("docs/i18n/en/cloudflare-edge-token.md");
  assert.match(tokenDoc, /Core install vs optional artifact relay/);
  assert.match(tokenDoc, /must succeed on Workers Free without R2/);
  assert.match(tokenDoc, /a missing permission, not a broken token/);
  const triage = read("docs/i18n/en/troubleshooting.md");
  assert.match(triage, /403 while the token verifies as valid/);
});

test("R2 is an optional artifact-relay capability, not a core install requirement", () => {
  for (const rel of AGENT_INSTALL) {
    const doc = read(rel);
    assert.match(doc, /Core install does not require R2|核心安装不需要 R2|コアインストールに R2 は不要/);
    assert.match(doc, /no payment method|不需要绑卡|没绑卡|支払い方法は不要/);
    // The legacy "R2 write is required" contract must be gone.
    assert.doesNotMatch(doc, /R2 write is required|R2 写权限用于在 Worker 部署前/);
  }
  const template = read("edge/cloudflare/wrangler.user.example.toml");
  // The default template must not activate an R2 binding.
  assert.doesNotMatch(template, /^\[\[r2_buckets\]\]/m);
  assert.match(template, /# \[\[r2_buckets\]\]/);
  assert.match(template, /R2 is NOT part of the core install/);
});

test("device identity follows the dev_<26-char ULID> contract and never a hostname-derived WORKSTATION_ID", () => {
  for (const rel of AGENT_INSTALL) {
    const doc = read(rel);
    assert.match(doc, /dev_01ARZ3NDEKTSV4RRFFQ69G5FAV/);
    assert.match(doc, /26[- ]character|26 字符|26 文字/);
    assert.match(doc, /immutable `device_id`|不可变的 `device_id`|不変の `device_id`/);
    // Display name and identity stay separate.
    assert.match(doc, /display name|显示名|表示名/);
    // The old free-form hostname-derived WORKSTATION_ID instruction is gone.
    assert.doesNotMatch(doc, /hostname-derived `WORKSTATION_ID`|限制在 `\[A-Za-z0-9_\.\-\]`/);
    assert.match(doc, /Do \*\*not\*\* invent a `WORKSTATION_ID`|\*\*不要\*\*自造 `WORKSTATION_ID`|`WORKSTATION_ID`[^\n]*でっち上げてはいけません/);
  }
});

test("workers.dev recovery preserves public-origin and transport boundaries", () => {
  for (const rel of AGENT_INSTALL) {
    const doc = read(rel);
    // Assert stable concepts and commands, not one exact English/Chinese sentence.
    assert.match(doc, /\/health/);
    assert.match(doc, /workers\.dev/);
    assert.match(doc, /Cloudflare DNS/);
    assert.match(doc, /Google DNS/);
    assert.match(doc, /hosts/i);
    assert.match(doc, /Custom Domain/);
    assert.match(doc, /system DNS|系统 DNS|システム DNS/);
    assert.match(doc, /OAuth issuer|OAuth\/MCP public origin/);
    assert.match(doc, /public MCP origin|公開 MCP origin/);
    assert.match(doc, /shared Relay|共享 Relay|共有 Relay/);
    assert.match(doc, /Relay[^\n]*(?:last|最后)|最後に[^\n]*Relay/);
    assert.doesNotMatch(doc, /consistent across Worker OAuth, MCP, and Link WSS|Worker OAuth、MCP、Link WSS 全部使用同一个入口/);
  }

  const triage = read("docs/i18n/en/troubleshooting.md");
  assert.match(triage, /hostname/i);
  assert.match(triage, /same Worker/i);
});
