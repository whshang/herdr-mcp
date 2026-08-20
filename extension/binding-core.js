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

/** 唤醒模板渲染: {agent} {pane} {status} {output}。 */
export function buildWakeTemplate(template, fields) {
  const t = (template ?? "").trim();
  if (!t) return "";
  return t
    .replaceAll("{agent}", fields.agent ?? "")
    .replaceAll("{pane}", fields.pane ?? "")
    .replaceAll("{status}", fields.status ?? "")
    .replaceAll("{output}", (fields.output ?? "").slice(0, 4000));
}
