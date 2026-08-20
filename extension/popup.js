// popup.js — 绑定/状态 UI
const $ = (id) => document.getElementById(id);
const STATUS_COLOR = { idle: "#9ca3af", working: "#d97706", done: "#16a34a", blocked: "#dc2626", unknown: "#6b7280" };

async function bg(msg) {
  return new Promise((resolve) => {
    chrome.runtime.sendMessage(msg, (resp) => {
      if (chrome.runtime.lastError) resolve(null);
      else resolve(resp);
    });
  });
}

async function activeTab() {
  return new Promise((resolve) => {
    chrome.tabs.query({ active: true, currentWindow: true }, (t) => resolve(t[0] || null));
  });
}

let toastTimer = null;
function showToast(text, kind = "err") {
  const t = $("toast");
  t.textContent = text;
  t.className = kind;
  t.style.display = "block";
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => { t.style.display = "none"; }, 5000);
}

async function refresh() {
  const tab = await activeTab();
  const st = await bg({ type: "h2w_state", tabId: tab?.id }) || {};
  const agents = await bg({ type: "h2w_agents" });

  // 服务状态
  const srv = $("srvStatus");
  if (agents?.ok) {
    srv.innerHTML = `<span class="ok">● 在线</span> <span class="muted">${agents.agents?.length || 0} agents</span>`;
  } else if (agents?.status === 401) {
    srv.innerHTML = `<span class="err">● 401 token 不匹配</span>`;
    $("convActions").innerHTML = `<div class="err">选项页里填的 token 与服务器不符。终端运行 <code>herdr-mcp token</code> 复制后粘贴。</div>
      <div style="margin-top:6px"><button id="openOptsBtn">打开选项页</button></div>`;
    $("openOptsBtn")?.addEventListener("click", () => chrome.runtime.openOptionsPage());
  } else {
    srv.innerHTML = `<span class="err">● 不可达${agents?.status ? ` (HTTP ${agents.status})` : agents?.error ? ` (${agents.error})` : ""}</span>`;
  }
  $("enabled").checked = !!st.config?.enabled;

  // 当前会话
  const conv = $("convInfo"), actions = $("convActions");
  if (st.convInfo) {
    conv.textContent = `${st.convInfo.site} · ${st.convInfo.convKey}`;
    if (st.binding) {
      actions.innerHTML = `<div class="row"><span>已绑定 <b>${st.binding.pane}</b> <span class="dot" style="background:${STATUS_COLOR[st.binding.status] || "#6b7280"}"></span>${st.binding.status || ""}</span>
        <button class="danger" id="unbindBtn">解绑</button></div>`;
      $("unbindBtn").addEventListener("click", async () => {
        await bg({ type: "h2w_unbind", convKey: st.convInfo.convKey });
        refresh();
      });
    } else {
      actions.innerHTML = `<button id="bindBtn" class="primary" disabled>绑定 (先选 agent)</button>`;
    }
  } else {
    conv.textContent = "当前标签页不是支持的会话页";
    actions.innerHTML = `<div class="hintbox">请先在 <b>z.ai / DeepSeek / Claude / ChatGPT</b> 打开一个对话页,再点扩展图标绑定。
      当前 tab: ${(tab && tab.url || "?").slice(0, 60)}</div>`;
  }

  // agents 列表
  const box = $("agents");
  if (!agents?.ok || !agents.agents?.length) {
    box.textContent = agents?.error || "无 agent (herdr 没在跑?)";
  } else {
    box.innerHTML = "";
    for (const a of agents.agents) {
      const row = document.createElement("div");
      row.className = "agent";
      const name = a.name || "(unnamed)";
      row.innerHTML = `<span class="dot" style="background:${STATUS_COLOR[a.status] || "#6b7280"}"></span><b>${name}</b> <span class="muted">${a.pane}</span>
        <div class="meta">${a.status} · ${(a.cwd || "").split("/").slice(-2).join("/")}</div>`;
      row.addEventListener("click", async () => {
        if (!tab?.id) return;
        const r = await bg({ type: "h2w_bind", tabId: tab.id, pane: a.pane, agent: name, workspace_id: a.workspace });
        if (r?.ok) {
          showToast(`✓ 已绑定 ${a.pane} → 当前会话`, "ok");
          refresh();
        } else if (r?.error === "conversation-unavailable") {
          showToast("绑定失败: 当前标签页不是支持的会话页。\n请先在 z.ai/DeepSeek/Claude/ChatGPT 打开对话再绑定。");
        } else if (r?.error === "already-bound") {
          showToast("该会话已绑定其他 agent,先解绑再换");
        } else {
          showToast(`绑定失败: ${r?.error || "无响应"}`);
        }
      });
      box.appendChild(row);
    }
  }

  // 已绑定列表
  const bl = $("bindings");
  if (!st.bindings?.length) bl.textContent = "无";
  else {
    bl.innerHTML = "";
    for (const b of st.bindings) {
      const row = document.createElement("div");
      row.className = "row";
      row.innerHTML = `<span><span class="dot" style="background:${STATUS_COLOR[b.status] || "#6b7280"}"></span>${b.pane} <span class="muted">→ ${b.site}</span></span>
        <button class="danger" data-conv="${b.convKey}">解绑</button>`;
      row.querySelector("button").addEventListener("click", async () => {
        await bg({ type: "h2w_unbind", convKey: b.convKey });
        refresh();
      });
      bl.appendChild(row);
    }
  }
}

$("enabled").addEventListener("change", async (e) => {
  await bg({ type: "h2w_set_config", config: { enabled: e.target.checked } });
});
$("openOptions").addEventListener("click", (e) => { e.preventDefault(); chrome.runtime.openOptionsPage(); });

refresh();
