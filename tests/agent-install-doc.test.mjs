import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const read = (p) => readFileSync(path.join(ROOT, p), "utf8");

test("README gives the Agent one executable install sentence plus a short explanation", () => {
  const cases = [
    ["README.md", /## 一句话安装/, /raw\.githubusercontent\.com\/whshang\/herdr-mcp\/main\/docs\/i18n\/zh-CN\/agent-install\.md/, /Agent 会检查环境/],
    ["README.en.md", /## One-sentence install/, /raw\.githubusercontent\.com\/whshang\/herdr-mcp\/main\/docs\/i18n\/en\/agent-install\.md/, /The agent checks the machine/],
    ["README.ja.md", /## 一文でインストール/, /raw\.githubusercontent\.com\/whshang\/herdr-mcp\/main\/docs\/i18n\/ja\/agent-install\.md/, /Agent が環境確認/],
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

test("Agent install guides own Cloudflare-token handoff and direct release-artifact bootstrap", () => {
  const docs = [
    ["docs/i18n/en/agent-install.md", /optional/i],
    ["docs/i18n/zh-CN/agent-install.md", /可选/],
    ["docs/i18n/ja/agent-install.md", /任意/],
  ];
  for (const [rel, optional] of docs) {
    const doc = read(rel);
    assert.match(doc, /dash\.cloudflare\.com\/profile\/api-tokens/);
    assert.match(doc, /Edit Cloudflare Workers/);
    assert.match(doc, /Workers Scripts/);
    assert.match(doc, /CLOUDFLARE_API_TOKEN/);
    assert.match(doc, /herdr-mcp worker bootstrap/);
    assert.match(doc, /herdr-edge-<version>\.mjs/);
    assert.match(doc, /release manifest/);
    assert.match(doc, /artifact attestation/);
    assert.match(doc, /Cloudflare API/);
    assert.match(doc, /Workers R2 Storage/);
    assert.match(doc, optional);
    assert.match(doc, /workers\.dev/);
    assert.doesNotMatch(doc, /npx wrangler|wrangler deploy --config|wrangler secret put/);
  }
});

test("Agent install resolves fleet existence before any Cloudflare mutation", () => {
  const en = read("docs/i18n/en/agent-install.md");
  const zh = read("docs/i18n/zh-CN/agent-install.md");
  const ja = read("docs/i18n/ja/agent-install.md");

  assert.ok(en.indexOf("first Worker or existing fleet") >= 0);
  assert.ok(en.indexOf("first Worker or existing fleet") < en.indexOf("## 4. First Worker"));
  assert.ok(zh.indexOf("第一台 Worker 还是加入已有 fleet") >= 0);
  assert.ok(zh.indexOf("第一台 Worker 还是加入已有 fleet") < zh.indexOf("## 4. 第一台 Worker"));
  assert.ok(ja.indexOf("最初の Worker か既存 fleet") >= 0);
  assert.ok(ja.indexOf("最初の Worker か既存 fleet") < ja.indexOf("## 4. 最初の Worker"));

  for (const doc of [en, zh, ja]) {
    assert.match(doc, /herdr-mcp worker pair/);
    assert.match(doc, /herdr-mcp worker connect "<pairing-address>"/);
    assert.match(doc, /(?:default|默认|既定)[^\n]*--name|--name[^\n]*(?:default|默认|既定)/i);
    assert.match(doc, /random-suffixed Worker|随机后缀[^\n]*Worker|ランダムサフィックス[^\n]*Worker/);
    assert.match(doc, /first[- ](?:Worker|fleet)|第一(?:台|套)[^\n]*Worker|最初の Worker/i);
    assert.doesNotMatch(doc, /grep -Fqx|zsh -ic/);
  }

  for (const rel of ["docs/i18n/en/existing-worker-connect.md", "docs/i18n/zh-CN/existing-worker-connect.md", "docs/i18n/ja/existing-worker-connect.md"]) {
    const doc = read(rel);
    assert.match(doc, /--name[^\n]*(?:explicitly|明确|明示的)|(?:explicitly|明确|明示的)[^\n]*--name/i);
  }

  assert.doesNotMatch(en, /choose a machine-specific\/random-suffixed name instead/);
  assert.doesNotMatch(zh, /改用机器相关\/随机后缀名/);
});

test("user recovers a failed first-device bootstrap by rerunning it | Given activation failed after credentials were saved | When reading any language's agent-install guide | Then it instructs a same-machine rerun that fails closed and never restores deleted credentials", () => {
  for (const [rel, rerun, failClosed, noRestore, keepJournal] of [
    [
      "docs/i18n/en/agent-install.md",
      /rerun `worker bootstrap` on the same computer and OS account/,
      /Missing, revoked or mismatched credentials fail closed/,
      /revoked or deleted credentials cannot be restored/,
      /Keep the journal and credential/,
    ],
    [
      "docs/i18n/zh-CN/agent-install.md",
      /同一台电脑、同一 OS 账号下重跑 `worker bootstrap`/,
      /缺失、吊销或不匹配的凭据 fail closed/,
      /已吊销或删除的凭据无法通过此路径恢复/,
      /保留 journal 与凭据/,
    ],
    [
      "docs/i18n/ja/agent-install.md",
      /同じコンピュータと同じ OS アカウントで `worker bootstrap` を再実行/,
      /資格情報は fail closed で失敗/,
      /失効または削除された資格情報はこの経路では復元できない/,
      /journal と資格情報は保持/,
    ],
  ]) {
    const doc = read(rel);
    assert.match(doc, rerun, `${rel} must keep the same-machine rerun contract`);
    assert.match(doc, failClosed, `${rel} must keep credentials fail closed`);
    assert.match(doc, noRestore, `${rel} must not promise restoring deleted credentials`);
    assert.match(doc, keepJournal, `${rel} must keep the journal and credential on recovery`);
  }
});

test("user diagnoses a stalled first-device bootstrap from doctor | Given a deployed Worker with 200 health but failed bootstrap | When reading any language's troubleshooting guide | Then it points to the read-only bootstrap journal layer and forbids Worker/secret deletion or manual pairing", () => {
  for (const rel of [
    "docs/i18n/en/troubleshooting.md",
    "docs/i18n/zh-CN/troubleshooting.md",
    "docs/i18n/ja/troubleshooting.md",
  ]) {
    const doc = read(rel);
    assert.match(doc, /LAYER bootstrap-journal/, `${rel} must name the doctor layer`);
    assert.match(
      doc,
      /~\/\.config\/herdr-mcp\/bootstrap\/first-worker-v1\.json/,
      `${rel} must name the read-only journal path`,
    );
    assert.match(doc, /herdr-mcp worker bootstrap/, `${rel} must keep the rerun path`);
    assert.match(
      doc,
      /Never delete the Worker|绝不删除 Worker|Worker を削除したり/,
      `${rel} must forbid Worker deletion`,
    );
    assert.match(
      doc,
      /create a pairing manually|手动创建 pairing|pairing を手動で作成/,
      `${rel} must forbid manual pairing`,
    );
  }
});

test("Agent install keeps the bootstrap Token ephemeral without requiring generic DNS write", () => {
  for (const rel of ["docs/i18n/en/agent-install.md", "docs/i18n/zh-CN/agent-install.md", "docs/i18n/ja/agent-install.md"]) {
    const doc = read(rel);
    assert.match(doc, /CLOUDFLARE_API_TOKEN/);
    assert.match(doc, /shell history/);
    assert.match(doc, /generic DNS Write|通用 DNS Write|汎用 DNS Write/);
    assert.match(doc, /Cloudflare DNS/);
    assert.match(doc, /Google DNS/);
  }
});

test("Agent install stays concise while preserving the executable bootstrap contract", () => {
  for (const rel of ["docs/i18n/en/agent-install.md", "docs/i18n/zh-CN/agent-install.md", "docs/i18n/ja/agent-install.md"]) {
    const doc = read(rel);
    assert.ok(doc.length < 10_000, `${rel} should stay a compact execution contract`);
    assert.match(doc, /GitHub Releases?/);
    assert.match(doc, /herdr-mcp install/);
    assert.doesNotMatch(doc, /## 2\.[^\n]*\n[\s\S]*?git clone https:\/\/github\.com\/whshang\/herdr-mcp/);
    assert.match(doc, /herdr-mcp worker bootstrap/);
    assert.doesNotMatch(doc, /scripts\/cloudflare-worker-name\.mjs/);
    assert.doesNotMatch(doc, /npx wrangler/);
    assert.match(doc, /Cloudflare[^\n]*API/);
    assert.match(doc, /extension/);
    assert.match(doc, /Native Messaging/);
    assert.match(doc, /HERDR_MCP_TOKEN/);
    assert.doesNotMatch(doc, /native-host use standalone|HERDR_LINK_PROXY|STANDALONE|v0\.4\.2/);
  }
  for (const rel of ["docs/i18n/en/install.md", "docs/i18n/zh-CN/install.md", "docs/i18n/ja/install.md"]) {
    const doc = read(rel);
    assert.match(doc, /herdr-mcp worker bootstrap/);
    assert.doesNotMatch(doc, /scripts\/cloudflare-worker-name\.mjs/);
    assert.doesNotMatch(doc, /npx wrangler/);
    assert.match(doc, /herdr-mcp install/);
  }
  for (const rel of ["README.md", "README.en.md", "README.ja.md"]) {
    const doc = read(rel);
    assert.match(doc, /raw\.githubusercontent\.com\/whshang\/herdr-mcp\/main\/docs\/i18n\//);
    assert.match(doc, /Chrome Web Store/);
    assert.doesNotMatch(doc, /scripts\/cloudflare-worker-name\.mjs/);
    assert.doesNotMatch(doc, /## (?:Local runtime CLI|本机 runtime CLI)/);
  }
});

test("quick Agent protocols plan once, batch safe work, and keep optional details out of the hot path", () => {
  const docs = [
    ["docs/i18n/en/agent-install.md", { plan: /Plan before calling tools/, bounded: /one bounded execution call/, replan: /Re-plan only/, sourceDev: /source development/, network: /Do not change the user's network environment/, customDomain: /custom domain/i, herdrRecovery: /owns Herdr dependency recovery/ }],
    ["docs/i18n/zh-CN/agent-install.md", { plan: /先规划，再调用/, bounded: /一个有界执行调用/, replan: /只有当结果会改变/, sourceDev: /源码开发/, network: /不修改用户网络环境/, customDomain: /自定义域名|Custom Domain/i, herdrRecovery: /负责 Herdr 依赖恢复/ }],
    ["docs/i18n/ja/agent-install.md", { plan: /ツールを呼ぶ前に計画する/, bounded: /一つの有界な実行呼び出し/, replan: /再計画は、結果が/, sourceDev: /ソース開発|ソース checkout/, network: /ユーザーのネットワーク環境を変更しない/, customDomain: /カスタムドメイン|Custom Domain/, herdrRecovery: /Herdr 依存関係の復旧を担当/ }],
  ];
  for (const [rel, markers] of docs) {
    const doc = read(rel);
    assert.doesNotMatch(doc, /herdr\.dev\/install\.(?:sh|ps1)/);
    assert.match(doc, /GitHub Releases?/);
    assert.match(doc, /herdr-mcp install/);
    assert.match(doc, markers.herdrRecovery);
    assert.match(doc, markers.plan);
    assert.match(doc, markers.bounded);
    assert.match(doc, markers.replan);
    assert.doesNotMatch(doc, /herdr-mcp-extension/);
    assert.doesNotMatch(doc, /cp -R extension|ln -s .*extension/);
    assert.match(doc, markers.sourceDev);
    assert.match(doc, markers.network);
    assert.match(doc, /herdr-edge-device\.username\.workers\.dev\/mcp/);
    assert.match(doc, /herdr-mcp\.example\.com\/mcp/);
    assert.match(doc, /workers\.dev/);
    assert.match(doc, markers.customDomain);
    assert.doesNotMatch(doc, /HERDR_LINK_PROXY|HTTPS_PROXY|ALL_PROXY|native-host use standalone/);
    assert.doesNotMatch(doc, /second-mac-agent-prompt/);
  }
});

test("legacy Node herdr-link wrapper resolves Node from PATH when explicitly invoked", () => {
  const src = read("bin/herdr-link");
  assert.match(src, /HERDR_NODE_BIN/);
  assert.match(src, /command -v node/);
  assert.doesNotMatch(src, /^NODE_BIN="\/usr\/local\/bin\/node"$/m);
});

test("end-user install docs exclude the retired second-Mac GA UAT prompt", () => {
  for (const rel of ["docs/i18n/en/install.md", "docs/i18n/zh-CN/install.md", "docs/i18n/ja/install.md"]) {
    assert.doesNotMatch(read(rel), /second-mac-ga-uat-agent-prompt/);
  }
});

test("browser privacy policy matches the Store-first extension data model", () => {
  for (const rel of ["docs/i18n/en/privacy.md", "docs/i18n/zh-CN/privacy.md", "docs/i18n/ja/privacy.md"]) {
    const doc = read(rel);
    assert.match(doc, /chrome\.storage\.local/);
    assert.match(doc, /Native Messaging|nativeMessaging/);
    assert.match(doc, /OpenAI-compatible|OpenAI 互換/);
    assert.match(doc, /Limited Use/);
    assert.match(doc, /remote executable code|远程可执行代码|リモートの実行可能コード/);
    assert.match(doc, /credit|信用/);
    assert.match(doc, /`debugger`/);
    assert.match(doc, /navigation history|导航历史|ナビゲーション履歴/);
  }
});
