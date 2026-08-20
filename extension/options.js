// options.js — 配置页
const $ = (id) => document.getElementById(id);
const KEYS = ["herdrMcpUrl", "token", "wakeTemplate", "autoAllow", "enabled"];

chrome.storage.local.get(KEYS, (cfg) => {
  $("url").value = cfg.herdrMcpUrl || "http://127.0.0.1:8772";
  $("token").value = cfg.token || "";
  $("template").value = cfg.wakeTemplate || "herdr agent {agent} ({pane}) 已完成 ({status})。\n\n{output}\n\n请基于以上结果继续。";
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
