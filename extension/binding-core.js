// binding-core.js — 绑定状态机与纯逻辑 (无 chrome API, 可被 Node 单测直接 import)
//
// 唤醒语义 (advisor 修正): 绑定 ≠ 立即唤醒。
//   - 初始 hello: 记录当前 seq/status 作为基线, 不唤醒 (绑定到已 idle/done 的 agent
//     不应立刻打扰网页)。
//   - agent_working: armed (persisted status="working")。
//   - agent_settled: 仅当 persisted status==="working" (工作确实发生过) 才唤醒;
//     同一 seq 不去重 (lastSettle 防重连/hello 重复)。
//   - 重连 hello: persisted status==="working" 且快照已 settled 且 seq 变化 →
//     补一次唤醒 (离线期间错过的 settle 恢复)。

export const SETTLED_STATUSES = ["idle", "done", "blocked"];

/**
 * 唤醒决策纯函数。
 * @param {{status: string|null, lastSettle: {seq: any, at: number}|null}} prev
 * @param {"hello"|"working"|"settled"} kind
 * @param {object} data — hello: {agent:{status,seq}}; settled: {status, seq, at}
 * @returns {{wake: boolean, status: string|null, lastSettle: {seq:any,at:number}|null}}
 */
export function decideWake(prev, kind, data) {
  const status = prev?.status ?? null;
  const lastSettle = prev?.lastSettle ?? null;
  const settled = (s) => SETTLED_STATUSES.includes(s);

  if (kind === "hello") {
    const ag = data?.agent ?? null;
    if (!ag) return { wake: false, status, lastSettle };
    if (ag.status === "working") return { wake: false, status: "working", lastSettle };
    if (!settled(ag.status)) return { wake: false, status: ag.status, lastSettle };
    // 快照 settled:
    //  - 之前正在工作 (离线错过的 settle) 且 seq 未通知过 → 补唤醒
    //  - 否则只是基线记录 / 已通知过 → 不唤醒
    const seq = ag.seq ?? `hello:${ag.status}`;
    if (status === "working" && (!lastSettle || lastSettle.seq !== seq)) {
      return { wake: true, status: ag.status, lastSettle: { seq, at: Date.now() } };
    }
    return { wake: false, status: ag.status, lastSettle };
  }

  if (kind === "working") {
    return { wake: false, status: "working", lastSettle };
  }

  if (kind === "settled") {
    if (!settled(data?.status)) return { wake: false, status, lastSettle };
    const seq = data.seq ?? `t:${data.at}`;
    if (seq != null && lastSettle?.seq === seq) return { wake: false, status: data.status, lastSettle };
    const wake = status === "working"; // 只有确实从工作态迁移过来才唤醒
    return { wake, status: data.status, lastSettle: { seq, at: Date.now() } };
  }

  return { wake: false, status, lastSettle };
}

/** 过期绑定清理: 返回保留集合 + 被剪枝的 convKey 列表。 */
export function pruneExpired(bindings, now = Date.now()) {
  const kept = {};
  const prunedKeys = [];
  for (const [k, b] of Object.entries(bindings)) {
    if (typeof b?.expires_at === "number" && b.expires_at <= now) prunedKeys.push(k);
    else kept[k] = b;
  }
  return { kept, prunedKeys };
}

/** 绑定 revision (内容寻址, 绑定变更检测用)。 */
export function bindingRevision(b) {
  const s = `${b.pane}\0${b.convKey}\0${b.created_at}`;
  let h = 0;
  for (let i = 0; i < s.length; i++) { h = (h * 31 + s.charCodeAt(i)) | 0; }
  return `h2w:${(h >>> 0).toString(16)}`;
}

/**
 * 进度 tick 决策纯函数 (主线 A): 是否该在本次定时器到期时向网页通报进度。
 * 用于 background 的 setInterval 回调 (now=Date.now()), 避免真实定时器竞态/叠加。
 *
 * 规则:
 *   - progressTickSec <= 0 (或非数字): 关闭, 永不 tick
 *   - prev.status !== "working": 不 tick (settled 走收工唤醒)
 *   - 距上次 tick (armed 时 = armedAt) 未满 progressTickSec: 不 tick
 *   - 满间隔: tick (仅表示「到检查点」, 是否真发消息见 shouldSendProgress)
 *
 * @param {{status: string|null, lastTickAt: number|null}} prev
 * @param {number} now Date.now()
 * @param {{progressTickSec: number}} cfg
 * @returns {boolean}
 */
export function shouldProgressTick(prev, now, cfg) {
  const sec = Number(cfg?.progressTickSec);
  if (!Number.isFinite(sec) || sec <= 0) return false; // 关闭
  if (!prev || prev.status !== "working") return false; // 非工作不 tick
  const lastTickAt = prev.lastTickAt ?? null;
  if (typeof lastTickAt !== "number") return false; // 无基线
  return now - lastTickAt >= sec * 1000;
}

/**
 * 进度摘要指纹: 去掉 spinner / 跑表时间 / 空白后再比, 避免「看起来一样」却每分钟当 new_output。
 */
export function progressOutputFingerprint(output) {
  return String(output ?? "")
    .replace(/\u001b\[[0-9;]*[A-Za-z]/g, "")
    .replace(/[⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏◐◓◑◒■□▪▫•●○◎◉]+/g, "")
    .replace(/\b\d{1,2}:\d{2}(:\d{2})?\b/g, "")
    .replace(/\b\d+[ms]\b/gi, "")
    .replace(/\s+/g, " ")
    .trim()
    .slice(-1200);
}

/**
 * 是否真的向网页发一条进度消息 (检查点通过之后)。
 *
 * 规则:
 *   - progressTickSec 只决定「多久检查一次」, 不决定发送节奏
 *   - 一旦已经实发过 (hasProgressSent), 距 lastSentAt 未满 progressFallbackSec → 一律 skip
 *     (底线从「最后一次发送」起算, 不是从武装/整点起的固定 cron)
 *   - 未满底线之外: 指纹有变 → new_output; 满底线且无新指纹 → fallback
 *
 * lastSentAt: 武装时 = now (尚未实发); 每次实发后更新。
 * hasProgressSent: 本轮 working 是否已向网页发过进度 (含空 output 的 fallback)。
 *
 * @param {{lastSentAt: number|null, lastOutputSent: string, hasProgressSent?: boolean}} prev
 * @param {number} now
 * @param {string} output 本轮摘要
 * @param {{progressFallbackSec?: number}} cfg
 * @returns {{send: boolean, reason: "new_output"|"fallback"|"skip"}}
 */
export function shouldSendProgress(prev, now, output, cfg) {
  const out = String(output ?? "").trim();
  const fp = progressOutputFingerprint(out);
  const prevFp = progressOutputFingerprint(prev?.lastOutputSent ?? "");
  const lastSentAt = prev?.lastSentAt;
  const fallbackSec = Number(cfg?.progressFallbackSec);
  const fallbackMs = Number.isFinite(fallbackSec) && fallbackSec > 0 ? fallbackSec * 1000 : 0;
  const hasProgressSent = prev?.hasProgressSent === true;

  // 已实发过 → 底线从最后一次发送起算; 未满一律不发 (含「又有一点新 output」)
  if (hasProgressSent && typeof lastSentAt === "number" && fallbackMs > 0 && now - lastSentAt < fallbackMs) {
    return { send: false, reason: "skip" };
  }

  if (fp.length > 0 && fp !== prevFp) {
    return { send: true, reason: "new_output" };
  }
  if (fallbackMs > 0 && typeof lastSentAt === "number" && now - lastSentAt >= fallbackMs) {
    return { send: true, reason: "fallback" };
  }
  return { send: false, reason: "skip" };
}

/** 唤醒模板渲染: {agent} {pane} {status} {output}。空 output 时压掉多余空行。 */
export function buildWakeTemplate(template, fields) {
  const t = (template ?? "").trim();
  if (!t) return "";
  const output = String(fields.output ?? "").slice(0, 4000).trim();
  let out = t
    .replaceAll("{agent}", fields.agent ?? "")
    .replaceAll("{pane}", fields.pane ?? "")
    .replaceAll("{status}", fields.status ?? "")
    .replaceAll("{output}", output);
  // 模板常写 `\n\n{output}\n\n`; output 为空时会留下大段空行
  out = out.replace(/[ \t]+\n/g, "\n").replace(/\n{3,}/g, "\n\n").trim();
  return out;
}
