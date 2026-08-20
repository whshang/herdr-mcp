#!/usr/bin/env node
/**
 * extension_smoke.mjs — 扩展静态检查 + 纯逻辑单测 (不依赖 Chrome)
 *
 * 1. manifest 引用的每个文件存在 + manifest 是合法 JSON
 * 2. 所有 JS 文件通过 node --check
 * 3. binding-core 状态机: 初始 settled 基线不唤醒 / working→settled 唤醒一次 /
 *    同 seq 不去重 / 重连 hello 补唤醒 / 过期剪枝 / 模板渲染
 * 4. speaks-json.js (vm 加载, 假 window): 嵌套括号 / 转义字符串的 tool-call 解析
 *
 * Usage: node tests/extension_smoke.mjs
 */
import { readFileSync, existsSync } from "node:fs";
import { spawnSync } from "node:child_process";
import vm from "node:vm";
import * as path from "node:path";
import { fileURLToPath } from "node:url";
import {
  decideWake, pruneExpired, bindingRevision, buildWakeTemplate,
} from "../extension/binding-core.js";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const EXT = path.join(__dirname, "..", "extension");
let failures = 0;
function ok(cond, label, detail = "") {
  if (cond) console.log(`  ✅ ${label}`);
  else { failures++; console.error(`  ❌ ${label} ${detail}`); }
}

// ---- 1. manifest 引用完整性 ----
const manifest = JSON.parse(readFileSync(path.join(EXT, "manifest.json"), "utf8"));
const referenced = [];
for (const cs of manifest.content_scripts || []) for (const js of cs.js || []) referenced.push(js);
referenced.push(manifest.background?.service_worker);
referenced.push(manifest.options_page);
referenced.push(manifest.action?.default_popup);
for (const [k, v] of Object.entries(manifest.icons || {})) referenced.push(v);
for (const [k, v] of Object.entries(manifest.action?.default_icon || {})) referenced.push(v);
for (const r of referenced) {
  if (!r) continue;
  ok(existsSync(path.join(EXT, r)), `manifest 引用文件存在: ${r}`);
}
ok(manifest.background?.type === "module", "background 是 module worker (import binding-core)");
ok(manifest.content_scripts.length === 4, "4 个站点 content_scripts");

// ---- 2. JS 语法 (固定清单) ----
const fixed = ["background.js", "options.js", "popup.js", "content/base.js",
  "content/injector/zai.js", "content/injector/deepseek.js", "content/injector/claude.js",
  "content/injector/chatgpt.js", "content/webmcp/speaks-json.js", "content/wake.js", "binding-core.js"];
for (const f of fixed) {
  const p = path.join(EXT, f);
  const r = spawnSync(process.execPath, ["--check", p], { encoding: "utf8" });
  ok(r.status === 0, `node --check ${f}`, r.stderr?.slice(0, 200));
}

// ---- 3. binding-core 状态机 ----
console.log("\n[decideWake]");
const none = { status: null, lastSettle: null };
// 初始 hello 已 settled → 基线记录, 不唤醒
let d = decideWake(none, "hello", { agent: { status: "idle", seq: 10 } });
ok(!d.wake && d.status === "idle" && d.lastSettle === null, "初始 hello settled → 基线, 不唤醒");
// working → armed
d = decideWake({ status: "idle", lastSettle: null }, "working", { status: "working" });
ok(!d.wake && d.status === "working", "working → armed, 不唤醒");
// settled after working → 唤醒一次
d = decideWake({ status: "working", lastSettle: null }, "settled", { status: "done", seq: 11 });
ok(d.wake && d.status === "done" && d.lastSettle.seq === 11, "working→settled → 唤醒一次");
// 同 seq 重复 → 不唤醒
d = decideWake({ status: "done", lastSettle: { seq: 11, at: 1 } }, "settled", { status: "done", seq: 11 });
ok(!d.wake, "同 seq settle 重复 → 不唤醒");
// 新 seq settle (再次工作) → 唤醒
d = decideWake({ status: "working", lastSettle: { seq: 11, at: 1 } }, "settled", { status: "idle", seq: 12 });
ok(d.wake, "新 seq settle → 唤醒");
// 绑定后从未见 working 的 settle → 不唤醒 (未 armed)
d = decideWake({ status: "idle", lastSettle: null }, "settled", { status: "done", seq: 13 });
ok(!d.wake, "未 armed 的 settle → 不唤醒");
// 重连 hello: persisted working + 快照 settled + 新 seq → 补唤醒
d = decideWake({ status: "working", lastSettle: { seq: 11, at: 1 } }, "hello", { agent: { status: "done", seq: 14 } });
ok(d.wake, "重连 hello (working→settled, 新 seq) → 补唤醒");
// 重连 hello: 同 seq → 不补
d = decideWake({ status: "done", lastSettle: { seq: 14, at: 2 } }, "hello", { agent: { status: "done", seq: 14 } });
ok(!d.wake, "重连 hello 同 seq → 不补");
// 重连 hello: snapshot 仍 working → 保持 armed
d = decideWake({ status: "working", lastSettle: null }, "hello", { agent: { status: "working", seq: 15 } });
ok(!d.wake && d.status === "working", "hello 仍 working → 保持 armed");

console.log("\n[pruneExpired / revision / template]");
const now = Date.now();
const { kept, prunedKeys } = pruneExpired({
  fresh: { expires_at: now + 1000 },
  stale: { expires_at: now - 1 },
  noexp: { pane: "x" },
}, now);
ok(Object.keys(kept).length === 2 && prunedKeys.length === 1 && prunedKeys[0] === "stale", "过期剪枝 (含无 expires_at 保留)");
ok(/^h2w:[0-9a-f]+$/.test(bindingRevision({ pane: "wH:p1", convKey: "https://chat.z.ai/chat/s/1", created_at: 1 })), "bindingRevision 格式");
ok(buildWakeTemplate("a {agent} {pane} {status} {output}", { agent: "pi", pane: "wH:p1", status: "done", output: "hello\nworld" }).includes("hello\nworld"), "模板渲染保留 output 换行");

// ---- 4. speaks-json.js 解析 (vm + 假 window) ----
console.log("\n[speaks-json extractToolCalls]");
{
  const code = readFileSync(path.join(EXT, "content/webmcp/speaks-json.js"), "utf8");
  const window = {};
  window.__H2W_ADAPTER__ = { name: "z.ai" };
  const ctx = vm.createContext({ window, document: { querySelectorAll: () => [] }, console });
  vm.runInContext(code, ctx);
  const sj = window.__H2W_SPEAKS_JSON__;
  ok(!!sj && sj.enabled === true, "vm 加载 speaks-json, z.ai 启用");
  // 嵌套括号 (apply_patch 含 {}) + 转义字符串
  const calls = sj.extractToolCalls(
    `前置文字 {"tool":"apply_patch","args":{"patch":"diff --git a/x b/x\\n@@ -1 +1 @@\\n-{\\"a\\":1}"}} 后置文字 {"tool":"exec_command","args":{"cmd":"echo hi"}}`,
  );
  ok(calls.length === 2, `解析出 2 个 tool call`, JSON.stringify(calls));
  ok(calls[0].tool === "apply_patch" && calls[0].args.patch.includes("{\"a\":1}"), "嵌套括号 + 转义还原正确");
  ok(calls[1].tool === "exec_command", "第二个调用正确");
  // 非工具对象跳过 / 未闭合停止
  const mixed = sj.extractToolCalls(`{"tool":"read_file","args":{"path":"a"}} {"not_a_tool":1} {"tool":"list_dir","args":`);
  ok(mixed.length === 1 && mixed[0].tool === "read_file", "跳过非工具对象, 未闭合停止");
  ok(sj.extractToolCalls(null).length === 0 && sj.extractToolCalls("").length === 0, "空输入安全");
}

console.log("\n[permission auto-allow 判定]");
{
  // vm 加载 base.js (classic script, 定义 isPermissionDialogText/isAllowButtonText)
  const code = readFileSync(path.join(EXT, "content/base.js"), "utf8");
  const window = {};
  const ctx = vm.createContext({ window, document: { querySelectorAll: () => [] }, console });
  vm.runInContext(code, ctx);
  const fn = (name) => vm.runInContext(name, ctx);
  ok(fn("isPermissionDialogText('ChatGPT 请求权限以使用工具')") === true, "权限弹窗文本识别 (中文)");
  ok(fn("isPermissionDialogText('ChatGPT needs your permission to use tools')") === true, "权限弹窗文本识别 (英文)");  ok(fn("isPermissionDialogText('这是一个普通对话框')") === false, "无权限字样不识别");
  ok(fn("isAllowButtonText('允许')") === true, "肯定按钮: 允许");
  ok(fn("isAllowButtonText('Allow')") === true, "肯定按钮: Allow");
  ok(fn("isAllowButtonText('同意并继续')") === true, "肯定按钮: 同意并继续");
  ok(fn("isAllowButtonText('拒绝')") === false, "拒绝按钮不点");
  ok(fn("isAllowButtonText('取消')") === false, "取消按钮不点");
  ok(fn("isAllowButtonText('Deny')") === false, "Deny 不点");
  ok(fn("isAllowButtonText('不要允许')") === false, "否定句不点");
}

console.log(`\n=== ${failures === 0 ? "EXTENSION SMOKE ALL PASS" : failures + " FAILURES"} ===`);
process.exit(failures === 0 ? 0 : 1);
