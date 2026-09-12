import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { DOC_ORDER } from "../scripts/site-i18n.mjs";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
const read = (rel) => readFileSync(join(ROOT, rel), "utf8");

function numberField(source, field) {
  const match = source.match(new RegExp(`["']?${field}["']?\\s*:\\s*(\\d+)`));
  assert.ok(match, `missing numeric ${field}`);
  return Number(match[1]);
}

function protocolVersions(handler) {
  const legacy = handler.match(/MCP_LEGACY_PROTOCOL\s*=\s*"([^"]+)"/);
  const block = handler.match(/MCP_SUPPORTED_PROTOCOLS\s*=\s*\[([\s\S]*?)\]\s*as const/);
  assert.ok(legacy, "missing MCP_LEGACY_PROTOCOL");
  assert.ok(block, "missing MCP_SUPPORTED_PROTOCOLS");
  const literals = [...block[1].matchAll(/"([0-9]{4}-[0-9]{2}-[0-9]{2})"/g)].map((match) => match[1]);
  return [legacy[1], ...literals];
}

test("published support matrix tracks the live protocol compatibility identities", () => {
  const en = read("docs/i18n/en/platform-support-matrix.md");
  const zh = read("docs/i18n/zh-CN/platform-support-matrix.md");
  const handler = read("edge/cloudflare/src/mcp-handler.ts");
  const epoch1 = read("edge/cloudflare/src/contracts/epoch1.ts");
  const epoch2 = read("edge/cloudflare/src/contracts/epoch2.ts");
  const epoch3 = read("edge/cloudflare/src/contracts/epoch3.ts");
  const runtime = read("edge/cloudflare/src/contracts/runtime.ts");
  const relay = read("src/relay/protocol.ts");

  const versions = protocolVersions(handler);
  for (const doc of [en, zh]) {
    for (const version of versions) assert.match(doc, new RegExp(version.replaceAll("-", "\\-")));
  }

  const probe = handler.match(/OPENAI_PROBE_PROTOCOL\s*=\s*"([^"]+)"/);
  assert.ok(probe, "missing OPENAI_PROBE_PROTOCOL");
  assert.ok(en.includes(probe[1]));
  assert.ok(zh.includes(probe[1]));
  assert.match(handler, /function negotiateProtocolVersion[\s\S]*return MCP_LEGACY_PROTOCOL;/);
  assert.match(en, /negotiate down to the explicit legacy baseline `2025-11-25`/);
  assert.match(zh, /明确降到 legacy baseline `2025-11-25`/);

  const identities = [
    [epoch1, en, zh],
    [epoch2, en, zh],
    [epoch3, en, zh],
  ];
  for (const [source, ...docs] of identities) {
    const epoch = numberField(source, "contract_epoch");
    const count = numberField(source, "tool_count");
    for (const doc of docs) {
      assert.match(doc, new RegExp(`epoch \\*\\*${epoch}\\*\\*`));
      assert.match(doc, new RegExp(`\\*\\*${count}\\*\\*`));
    }
  }

  assert.match(runtime, /COMPATIBLE_RUNTIME_CONTRACTS\s*=\s*\[EPOCH2_CONTRACT, EPOCH1_CONTRACT\]/);
  assert.match(en, /N-1 runtime execution[\s\S]*epoch-1/i);
  assert.match(zh, /N-1 runtime execution[\s\S]*epoch-1/i);
  assert.match(relay, /protocol_version` is the number `1` on every message/);
  assert.match(en, /protocol_version = 1/);
  assert.match(zh, /protocol_version = 1/);
});

test("matrix records rollback-safe durable-state policy instead of promising arbitrary N+1", () => {
  const en = read("docs/i18n/en/platform-support-matrix.md");
  const zh = read("docs/i18n/zh-CN/platform-support-matrix.md");
  const updater = read("crates/herdr-mcp/src/updater.rs");
  const store = read("crates/herdr-mcp/src/state_store.rs");

  assert.match(updater, /release state schema is not rollback-compatible with local schema/);
  assert.match(updater, /release manifest contract identity mismatch/);
  assert.match(store, /higher[\s\S]*refused \(fail-closed, no silent downgrade\)/);

  for (const doc of [en, zh]) {
    assert.match(doc, /N\+1/);
    assert.match(doc, /state schema/i);
    assert.match(doc, /fail|拒绝|失败关闭/i);
  }
});

test("matrix publishes explicit production, candidate, and unsupported platform boundaries", () => {
  const en = read("docs/i18n/en/platform-support-matrix.md");
  const zh = read("docs/i18n/zh-CN/platform-support-matrix.md");

  assert.match(en, /\| macOS \| \*\*Production\*\*/);
  assert.match(en, /\| Linux \(native Debian-class host\) \| \*\*Production\*\*/);
  assert.match(en, /\| Windows x86_64 \| \*\*Candidate — not production\*\*/);
  assert.match(en, /\| WSL \| \*\*Unsupported\*\*/);
  assert.match(en, /pull\/364/);
  assert.match(en, /issues\/394/);
  assert.match(en, /Source merge and hosted CI are qualification evidence, not a production guarantee/);
  assert.match(en, /UNC\/path behavior must be covered/i);

  assert.match(zh, /\| macOS \| \*\*生产支持\*\*/);
  assert.match(zh, /\| Linux（原生 Debian 类主机） \| \*\*生产支持\*\*/);
  assert.match(zh, /\| Windows x86_64 \| \*\*候选——非生产支持\*\*/);
  assert.match(zh, /\| WSL \| \*\*不支持\*\*/);
  assert.match(zh, /pull\/364/);
  assert.match(zh, /issues\/394/);
  assert.match(zh, /源码合并和 hosted CI 只是候选验证证据，不构成正式支持保证/);
  assert.match(zh, /UNC\/网络路径能力/);

  assert.ok(DOC_ORDER.includes("platform-support-matrix"), "support matrix must be published in site navigation");
});
