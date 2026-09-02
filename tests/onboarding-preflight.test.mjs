import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const read = (p) => readFileSync(path.join(ROOT, p), "utf8");

const AGENT_INSTALL = ["docs/i18n/en/agent-install.md", "docs/i18n/zh-CN/agent-install.md"];

test("onboarding resolves first-vs-existing fleet intent before any Cloudflare deploy", () => {
  for (const rel of AGENT_INSTALL) {
    const doc = read(rel);
    // One explicit question gates the deploy path.
    assert.match(doc, /existing Herdr Worker|加入已有 Herdr Worker/);
    // Pairing evidence routes to the existing-fleet flow.
    assert.match(doc, /pairing address/);
    assert.match(doc, /worker pair/);
    assert.match(doc, /worker connect/);
    // Never deploy a second Worker/R2/Connector for an existing fleet.
    assert.match(doc, /second Worker|R2 bucket|R2 桶/);
    assert.match(doc, /existing-worker-connect\.md/);
    // Pre-deploy existing-Worker detection via the Cloudflare API.
    assert.match(doc, /workers\/scripts/);
  }
  // The dedicated multi-device contract already forbids re-deployment.
  const fleet = read("docs/i18n/en/existing-worker-connect.md");
  assert.match(fleet, /do not deploy another Worker/);
});

test("PATH preflight separates the installed binary from the interactive-shell PATH and self-heals", () => {
  for (const rel of AGENT_INSTALL) {
    const doc = read(rel);
    assert.match(doc, /installed_but_not_on_shell_path/);
    assert.match(doc, /zsh -ic 'command -v herdr-mcp'/);
    assert.match(doc, /zsh -lc 'command -v herdr-mcp'/);
    // Self-heal first, durable fix second, and no second PATH owner.
    assert.match(doc, /export PATH="\$HOME\/\.local\/bin:\$PATH"/);
    assert.match(doc, /second PATH owner|第二个 PATH owner/);
  }
  const triage = read("docs/i18n/en/troubleshooting.md");
  assert.match(triage, /installed_but_not_on_shell_path/);
  assert.match(triage, /not a missing installation/);
});

test("macOS TCC/FDA readiness is verified before background setup, not after install", () => {
  for (const rel of AGENT_INSTALL) {
    const doc = read(rel);
    assert.match(doc, /herdr-mcp permissions verify/);
    assert.match(doc, /herdr-mcp permissions status/);
    assert.match(doc, /needs_setup/);
    assert.match(doc, /Full Disk Access|完全磁盘访问/);
    assert.match(doc, /before Cloudflare work|Cloudflare 工作之前/);
  }
});

test("Cloudflare token preflight distinguishes a valid token from a missing permission", () => {
  for (const rel of AGENT_INSTALL) {
    const doc = read(rel);
    assert.match(doc, /Account Settings → Read/);
    assert.match(doc, /Workers Scripts → Write\/Edit/);
    // 403 diagnosis maps the failing call to the exact permission.
    assert.match(doc, /workers\/subdomain.*403|403.*workers\/subdomain/);
    assert.match(doc, /do not inflate|不要无根据扩大权限/);
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
    assert.match(doc, /Core install does not require R2|核心安装不需要 R2/);
    assert.match(doc, /no payment method|没绑卡/);
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
    assert.match(doc, /26[- ]character|26 字符/);
    assert.match(doc, /immutable `device_id`|不可变的 `device_id`/);
    // Display name and identity stay separate.
    assert.match(doc, /display name|显示名/);
    // The old free-form hostname-derived WORKSTATION_ID instruction is gone.
    assert.doesNotMatch(doc, /hostname-derived `WORKSTATION_ID`|限制在 `\[A-Za-z0-9_\.\-\]`/);
    assert.match(doc, /Do \*\*not\*\* invent a `WORKSTATION_ID`|\*\*不要\*\*自造 `WORKSTATION_ID`/);
  }
});

test("origin health checks separate Worker-code health from hostname/DNS/network-path health", () => {
  for (const rel of AGENT_INSTALL) {
    const doc = read(rel);
    assert.match(doc, /GET \/health/);
    assert.match(doc, /[Hh]ostname\/DNS\/network-path|hostname\/DNS\/网络路径/);
    assert.match(doc, /never .*redeploy|绝不用重新部署 Worker/);
    // Custom Domain is preferred as production origin when the user owns one.
    assert.match(doc, /prefer a Custom Domain|优先把 Custom Domain/);
  }
  const triage = read("docs/i18n/en/troubleshooting.md");
  assert.match(triage, /one hostname fails while another hostname of the same Worker works/);
});

test("bare worker pair without --name must not serialize a null device name", () => {
  // The v0.4.4 fix on main owns this behavior via dedicated request-body
  // helpers; this lane only asserts the contract stays intact (no duplicate
  // implementation here).
  const src = read("crates/herdr-mcp/src/worker.rs");
  assert.match(src, /fn pairing_create_request_body/);
  assert.match(src, /fn pairing_consume_request_body/);
  // The regression shape must be gone from the raw call sites (the helpers
  // themselves legitimately build the named variant internally).
  assert.doesNotMatch(src, /\.json\(&json!\(\{ "ttl_seconds": ttl_seconds, "name": name \}\)/);
  assert.doesNotMatch(src, /\.json\(&json!\(\{ "pairing_id": pairing_id, "code": code, "name": name \}\)/);
});

