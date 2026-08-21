// options.js — 配置页
const $ = (id) => document.getElementById(id);
const KEYS = ["herdrMcpUrl", "token", "wakeTemplate", "progressTickSec", "progressFallbackSec", "progressTemplate", "autoAllow", "enabled"];
const DEFAULT_TEMPLATE = "herdr agent {agent} ({pane}) 已完成 ({status})。\n\n{output}\n\n请基于以上结果继续。";
const DEFAULT_PROGRESS_TEMPLATE = "herdr agent {agent} ({pane}) 仍在执行 ({status})。\n\n{output}\n\n请调用 herdr_since 或继续观察进度，不要停在本轮。";

/** 秒数解析: 非数字/缺失 → fallback 默认; 负数/NaN → 0 (关闭); 上限 86400。 */
function parseTickSec(v, fallback = 60) {
  const n = Number(v);
  if (v === "" || v === undefined || v === null) return fallback;
  if (!Number.isFinite(n)) return fallback;
  if (n <= 0) return 0;
  return Math.min(Math.floor(n), 86400);
}

chrome.storage.local.get(KEYS, (cfg) => {
  $("url").value = cfg.herdrMcpUrl || "http://127.0.0.1:8772";
  $("token").value = cfg.token || "";
  $("template").value = cfg.wakeTemplate || DEFAULT_TEMPLATE;
  $("progressTickSec").value = cfg.progressTickSec ?? 60;
  $("progressFallbackSec").value = cfg.progressFallbackSec ?? 600;
  $("progressTemplate").value = cfg.progressTemplate || DEFAULT_PROGRESS_TEMPLATE;
  $("autoAllow").checked = cfg.autoAllow !== false;
  $("enabled").checked = cfg.enabled !== false;
});

function setStatus(text, cls) {
  $("status").textContent = text;
  $("status").className = cls || "";
}

$("save").addEventListener("click", () => {
  const config = {
    herdrMcpUrl: $("url").value.trim(),
    token: $("token").value.trim(),
    wakeTemplate: $("template").value,
    progressTickSec: parseTickSec($("progressTickSec").value, 60),
    progressFallbackSec: parseTickSec($("progressFallbackSec").value, 600),
    progressTemplate: $("progressTemplate").value,
    autoAllow: $("autoAllow").checked,
    enabled: $("enabled").checked,
  };
  chrome.runtime.sendMessage({ type: "h2w_set_config", config }, (resp) => {
    if (chrome.runtime.lastError || !resp?.ok) setStatus("保存失败: " + (chrome.runtime.lastError?.message || ""), "err");
    else setStatus("✓ 已保存", "ok");
  });
});

$("test").addEventListener("click", () => {
  const url = $("url").value.trim().replace(/\/+$/, "");
  const token = $("token").value.trim();
  if (!url) { setStatus("请先填地址", "err"); return; }
  setStatus("测试中…", "");
  fetch(`${url}/push/state`, { headers: token ? { Authorization: `Bearer ${token}` } : {} })
    .then((r) => {
      if (r.ok) return r.json().then((j) => setStatus(`✓ 连接成功 (${j.agents?.length || 0} agents)`, "ok"));
      if (r.status === 401) {
        setStatus("✖ HTTP 401 — token 不匹配或为空。\n终端运行 `herdr-mcp token` 复制后粘贴(注意别带前缀/空格)。", "err");
      } else {
        setStatus(`✖ HTTP ${r.status}`, "err");
      }
    })
    .catch((e) => setStatus("✖ 不可达: " + e.message + "\n确认地址是 http://127.0.0.1:8772", "err"));
});
