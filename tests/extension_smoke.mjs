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

console.log("\n[tool-action 权限卡片 DOM 自动允许]");
{
  // 轻量自建 DOM fixture (不新增依赖): 支持 base.js __H2W_PERMISSION__ 用到的 API。
  // 仅实现 helper 实际使用的子集: parentElement / childNodes / querySelectorAll /
  // hasAttribute/getAttribute / matches / innerText / click 计数。
  class MockEl {
    constructor(tag, attrs = {}) {
      this.tagName = tag.toUpperCase();
      this.nodeType = 1;
      this.parentElement = null;
      this.childNodes = [];
      this.attrs = { ...attrs };
      this.clickCount = 0;
      this.isConnected = true;
      this.disabled = false;
      this.hidden = false;
    }
    get className() { return this.attrs.class || ""; }
    getAttribute(n) { return this.attrs[n] ?? null; }
    hasAttribute(n) { return n in this.attrs; }
    matches(sel) {
      const btnSel = "button, [role=button], [class*=btn]";
      if (sel !== btnSel) return false;
      return this.tagName === "BUTTON" || this.getAttribute("role") === "button"
        || (this.className || "").toLowerCase().includes("btn");
    }
    click() { this.clickCount++; }
    get innerText() {
      // 与浏览器一致的近似: 拼接文本子树 (跳过 aria-hidden 区不计, 此处简化为全部文本)
      let out = "";
      for (const c of this.childNodes) {
        if (c.nodeType === 3) out += c.data;
        else if (c.nodeType === 1) out += c.innerText;
      }
      return out;
    }
    get textContent() { return this.innerText; }
    querySelectorAll(sel) {
      const out = [];
      (function walk(n) {
        for (const c of n.childNodes || []) {
          if (c.nodeType === 1) {
            if (typeof c.matches === "function" && c.matches(sel)) out.push(c);
            walk(c);
          }
        }
      })(this);
      return out;
    }
  }
  function textNode(data) { return { nodeType: 3, data, innerText: data, textContent: data }; }
  function el(tag, attrs = {}, ...kids) {
    const e = new MockEl(tag, attrs);
    for (const k of kids) {
      if (typeof k === "string") e.childNodes.push(textNode(k));
      else { k.parentElement = e; e.childNodes.push(k); }
    }
    return e;
  }
  function btn(label, attrs = {}) {
    const b = el("button", attrs, label);
    b.disabled = !!attrs.disabled;
    b.hidden = !!attrs.hidden;
    return b;
  }
  // 文档根: body 作为 cardForButton 向上遍历的终点 (真实 DOM: btn.ownerDocument.body)
  function buildDoc(card) {
    const docEl = el("html", {});
    const body = el("body", {});
    body.childNodes.push(card); card.parentElement = body;
    docEl.childNodes.push(body); body.parentElement = docEl;
    const doc = { body, documentElement: docEl };
    (function setOwner(n) {
      for (const c of n.childNodes || []) {
        if (c.nodeType === 1) { c.ownerDocument = doc; setOwner(c); }
      }
    })(body);
    return { document: body, body, documentElement: docEl };
  }

  // 加载 base.js 到 vm, 取得 __H2W_PERMISSION__ 测试 hook
  const code = readFileSync(path.join(EXT, "content/base.js"), "utf8");
  const window = {};
  const ctx = vm.createContext({ window, document: { querySelectorAll: () => [] }, console });
  vm.runInContext(code, ctx);
  const P = vm.runInContext("window.__H2W_PERMISSION__", ctx);

  // 1) 新 tool-action card: 点主"允许"恰好 1 次
  {
    const allow = btn("允许");
    const deny = btn("拒绝");
    const drop = btn("", { "aria-haspopup": "menu", "aria-label": "更多操作" });
    const card = el("div", { class: "tool-action-card" },
      el("h3", {}, "ChatGPT 请求使用工具"),
      el("p", {}, "此工具需要权限访问你的文件"),
      el("div", { class: "btn-area", "data-testid": "tool-action-buttons" }, deny, allow, drop));
    const { document } = buildDoc(card);
    const clicker = P.createPermissionClicker();
    const r1 = clicker.tryClick(document);
    ok(r1.handled === true && r1.button === allow, "tool-action card 找到可点允许按钮");
    const r2 = clicker.tryClick(document);
    ok(r2.duplicate === true && r2.handled === false, "重复 mutation 不重复点击 (duplicate)");
    ok(allow.clickCount === 1, "允许按钮恰好点击 1 次");
    ok(deny.clickCount === 0, "拒绝按钮不点");
    ok(drop.clickCount === 0, "aria-haspopup=menu 下拉不点");
  }
  // 2) 下拉箭头单独存在时也不点 (卡片含拒绝+允许+下拉: 只点允许)
  {
    const allow = btn("允许");
    const deny = btn("拒绝");
    const drop = btn("", { "aria-haspopup": "menu", "aria-label": "更多操作" });
    const card = el("div", { class: "tool-action-card" },
      el("h3", {}, "ChatGPT 请求使用工具"),
      el("p", {}, "此工具需要权限"),
      el("div", { class: "btn" }, deny, allow, drop));
    const { document } = buildDoc(card);
    const clicker = P.createPermissionClicker();
    const r = clicker.tryClick(document);
    ok(r.handled === true && r.button === allow, "允许+下拉(含拒绝): 点允许不点下拉");
    ok(drop.clickCount === 0, "下拉箭头点击计数 0");
  }
  // 3) 无拒绝按钮 → 不点 (fail-closed)
  {
    const allow = btn("允许");
    const card = el("div", { class: "tool-action-card" },
      el("h3", {}, "ChatGPT 请求使用工具"),
      el("p", {}, "此工具需要权限"),
      el("div", { class: "btn" }, allow));
    const { document } = buildDoc(card);
    const clicker = P.createPermissionClicker();
    const r = clicker.tryClick(document);
    ok(r.handled === false && allow.clickCount === 0, "无拒绝按钮 → 不点");
  }
  // 4) 标题非权限 → 不点 (允许二字只在按钮里, 不把卡片判成权限)
  {
    const allow = btn("允许");
    const deny = btn("拒绝");
    const card = el("div", { class: "tool-action-card" },
      el("h3", {}, "保存确认"),
      el("div", { class: "btn" }, deny, allow));
    const { document } = buildDoc(card);
    const clicker = P.createPermissionClicker();
    const r = clicker.tryClick(document);
    ok(r.handled === false && allow.clickCount === 0, "标题非权限 → 不点");
  }
  // 5) disabled / hidden 允许按钮 → 不点
  {
    const allowDis = btn("允许", { disabled: true });
    const allowHid = btn("允许", { hidden: true });
    const deny = btn("拒绝");
    const card1 = el("div", { class: "tool-action-card" },
      el("h3", {}, "ChatGPT 请求使用工具"), el("p", {}, "需要权限"),
      el("div", { class: "btn" }, deny, allowDis));
    const card2 = el("div", { class: "tool-action-card" },
      el("h3", {}, "ChatGPT 请求使用工具"), el("p", {}, "需要权限"),
      el("div", { class: "btn" }, deny, allowHid));
    for (const [card, a, tag] of [[card1, allowDis, "disabled"], [card2, allowHid, "hidden"]]) {
      const { document } = buildDoc(card);
      const clicker = P.createPermissionClicker();
      const r = clicker.tryClick(document);
      ok(r.handled === false && a.clickCount === 0, `${tag} 允许按钮 → 不点`);
    }
  }
  // 6) 旧 role=dialog 仍可识别
  {
    const allow = btn("Allow");
    const deny = btn("Deny");
    const card = el("div", { role: "dialog", "aria-modal": "true" },
      el("h3", {}, "Grant permission"),
      el("p", {}, "Allow this tool to access your data"),
      el("div", { class: "btn" }, deny, allow));
    const { document } = buildDoc(card);
    const clicker = P.createPermissionClicker();
    const r = clicker.tryClick(document);
    ok(r.handled === true && r.button === allow && allow.clickCount === 1, "旧 role=dialog 仍可识别并点 Allow 1 次");
    ok(deny.clickCount === 0, "dialog 内 Deny 不点");
  }
  // 7) 无文本按钮 / aria-label=更多 的纯图标按钮不点
  {
    const iconBtn = btn("", { "aria-label": "更多操作" });
    const allow = btn("允许");
    const deny = btn("拒绝");
    const card = el("div", { class: "tool-action-card" },
      el("h3", {}, "ChatGPT 请求使用工具"), el("p", {}, "需要权限"),
      el("div", { class: "btn" }, iconBtn, deny, allow));
    const { document } = buildDoc(card);
    const clicker = P.createPermissionClicker();
    const r = clicker.tryClick(document);
    ok(r.handled === true && r.button === allow && iconBtn.clickCount === 0, "无文本/更多图标按钮不点");
  }
  // 8) aria-disabled=true / aria-hidden=true → 不点 (fail-closed)
  {
    const allowArDis = btn("允许", { "aria-disabled": "true" });
    const allowArHid = btn("允许", { "aria-hidden": "true" });
    const deny = btn("拒绝");
    const mkCard = (allow) => el("div", { class: "tool-action-card" },
      el("h3", {}, "ChatGPT 请求使用工具"), el("p", {}, "需要权限"),
      el("div", { class: "btn" }, deny, allow));
    for (const [allow, tag] of [[allowArDis, "aria-disabled=true"], [allowArHid, "aria-hidden=true"]]) {
      const { document } = buildDoc(mkCard(allow));
      const clicker = P.createPermissionClicker();
      const r = clicker.tryClick(document);
      ok(r.handled === false && allow.clickCount === 0, `${tag} 允许按钮 → 不点`);
    }
  }
  // 9) ChatGPT 卡外另有"允许"按钮: 仍只点 action area 主允许, 不取外部按钮
  {
    const mainAllow = btn("允许");
    const deny = btn("拒绝");
    const card = el("div", { class: "tool-action-card" },
      el("h3", {}, "ChatGPT 请求使用工具"), el("p", {}, "此工具需要权限"),
      el("div", { class: "btn-area" }, deny, mainAllow));
    // 外部孤立"允许"按钮 (在同一 body 下, 但其所在 action 区无拒绝 → 精确路径拒绝)
    const externalAllow = btn("允许");
    // 注意: 外部按钮 DOM 顺序在前, 确保 findAllowAction 先扫到它也跳过, 再选主允许
    const docEl = el("html", {});
    const body = el("body", {}, externalAllow, card);
    externalAllow.parentElement = body; card.parentElement = body;
    docEl.childNodes.push(body); body.parentElement = docEl;
    const doc = { body, documentElement: docEl };
    (function setOwner(n) {
      for (const c of n.childNodes || []) {
        if (c.nodeType === 1) { c.ownerDocument = doc; setOwner(c); }
      }
    })(body);
    const clicker = P.createPermissionClicker();
    const r = clicker.tryClick(body);
    ok(r.handled === true && r.button === mainAllow, "卡外另有允许: 仍选 action area 主允许");
    ok(mainAllow.clickCount === 1 && externalAllow.clickCount === 0, "外部允许按钮不被点");
  }
  // 10) 初始 disabled 允许按钮 → 移除属性后再次 tryClick (等价 Observer callback) 能补点, 随后去重
  {
    const allow = btn("允许", { disabled: true });
    const deny = btn("拒绝");
    const card = el("div", { class: "tool-action-card" },
      el("h3", {}, "ChatGPT 请求使用工具"), el("p", {}, "需要权限"),
      el("div", { class: "btn" }, deny, allow));
    const { document } = buildDoc(card);
    const clicker = P.createPermissionClicker();
    const r1 = clicker.tryClick(document);
    ok(r1.handled === false && allow.clickCount === 0, "初始 disabled → 不点");
    // 模拟站点后挂载 enabled: 移除 disabled 属性 + 清 disabled 标志 (observer callback 会再调 tryClick)
    delete allow.attrs.disabled;
    allow.disabled = false;
    const r2 = clicker.tryClick(document); // 等价 Observer callback 再跑一次
    ok(r2.handled === true && r2.button === allow && allow.clickCount === 1, "移除 disabled 后补点 1 次");
    const r3 = clicker.tryClick(document);
    ok(r3.duplicate === true && r3.handled === false && allow.clickCount === 1, "补点后再次 tryClick 去重 (不重复点击)");
  }
  // 11) 嵌套外层含 deny + 卡外 allow 的负例: 仍只点 exact action area 主允许
  //     卡内 action area 有 data-testid=tool-action-buttons; 外层再包一个含 deny 的容器,
  //     卡外另有一个孤立 allow。actionAreaFor 必须优先 testid 区, 不扩大到外层 deny。
  {
    const mainAllow = btn("允许");
    const innerDeny = btn("拒绝");
    const actionArea = el("div", { class: "btn-area", "data-testid": "tool-action-buttons" }, innerDeny, mainAllow);
    const card = el("div", { class: "tool-action-card" },
      el("h3", {}, "ChatGPT 请求使用工具"), el("p", {}, "此工具需要权限"), actionArea);
    // 外层更大的 deny 容器 (含卡片): 精确 testid 区应阻止扩大到这层
    const outerDeny = btn("拒绝");
    const outerWrap = el("div", { class: "outer" }, outerDeny, card);
    // 卡外孤立 allow (与卡片无关)
    const externalAllow = btn("允许");
    const docEl = el("html", {});
    const body = el("body", {}, externalAllow, outerWrap);
    externalAllow.parentElement = body; outerWrap.parentElement = body;
    docEl.childNodes.push(body); body.parentElement = docEl;
    const doc = { body, documentElement: docEl };
    (function setOwner(n) {
      for (const c of n.childNodes || []) {
        if (c.nodeType === 1) { c.ownerDocument = doc; setOwner(c); }
      }
    })(body);
    const clicker = P.createPermissionClicker();
    const r = clicker.tryClick(body);
    ok(r.handled === true && r.button === mainAllow, "嵌套外层含 deny: 仍点真实 testid action area 主允许");
    ok(mainAllow.clickCount === 1 && externalAllow.clickCount === 0 && outerDeny.clickCount === 0, "仅主允许被点 (外部/外层 deny 均不点)");
  }
  // 12) 无 data-testid 的语义 fallback: 最小含 deny 祖先仍可识别 (旧 dialog/无 testid 站)
  {
    const allow = btn("允许");
    const deny = btn("拒绝");
    const card = el("div", { class: "tool-action-card" },
      el("h3", {}, "ChatGPT 请求使用工具"), el("p", {}, "此工具需要权限"),
      el("div", { class: "btn" }, deny, allow)); // 无 data-testid
    const { document } = buildDoc(card);
    const clicker = P.createPermissionClicker();
    const r = clicker.tryClick(document);
    ok(r.handled === true && r.button === allow && allow.clickCount === 1, "无 data-testid: 语义 fallback 仍点主允许 1 次");
  }
}

console.log(`\n=== ${failures === 0 ? "EXTENSION SMOKE ALL PASS" : failures + " FAILURES"} ===`);
process.exit(failures === 0 ? 0 : 1);
