// binding-core.js — binding state machine and pure logic, importable by Node tests
//
// Wake semantics:
//   - Initial hello records seq/status as a baseline without waking.
//   - agent_working arms the binding with persisted status="working".
//   - agent_settled wakes only after observed work; lastSettle deduplicates repeats.
//   - A reconnecting hello wakes once when persisted working became settled offline.

import { chatGptConversationInfo } from "./continuity-core.js";

export const SETTLED_STATUSES = ["idle", "done", "blocked"];

/**
 * Derive a conservative conversation identity directly from a supported tab URL.
 * Used only as a recovery path when an MV3 content script is temporarily absent
 * (for example immediately after reloading the extension while the tab stays open).
 */
export function conversationInfoFromSupportedUrl(rawUrl) {
  return chatGptConversationInfo(rawUrl);
}

/**
 * Pure wake-decision function.
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
    // Settled snapshot: recover an unreported offline transition; otherwise update baseline.
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
    const wake = status === "working"; // Wake only after an observed working state.
    return { wake, status: data.status, lastSettle: { seq, at: Date.now() } };
  }

  return { wake: false, status, lastSettle };
}

/** Remove expired bindings and return retained entries plus pruned convKeys. */
export function pruneExpired(bindings, now = Date.now()) {
  const kept = {};
  const prunedKeys = [];
  for (const [k, b] of Object.entries(bindings)) {
    if (typeof b?.expires_at === "number" && b.expires_at <= now) prunedKeys.push(k);
    else kept[k] = b;
  }
  return { kept, prunedKeys };
}

/** Content-addressed binding revision, preferring workspace scope. */
export function bindingRevision(b) {
  const scope = b.workspace_id || b.pane || "";
  const s = `${scope}\0${b.convKey}\0${b.created_at}`;
  let h = 0;
  for (let i = 0; i < s.length; i++) { h = (h * 31 + s.charCodeAt(i)) | 0; }
  return `h2w:${(h >>> 0).toString(16)}`;
}

/** Filter agents belonging to a workspace id. */
export function agentsInWorkspace(agents, workspaceId) {
  if (!workspaceId) return [];
  return (agents || []).filter((a) => a && a.workspace === workspaceId);
}

/** Count agents still in working status. */
export function scopeWorkingCount(agents) {
  return (agents || []).filter((a) => a && a.status === "working").length;
}

const IDLE_LIKE = new Set(["idle", "done", "blocked"]);

/** Human-facing workspace title: prefer herdr label, else project folder, else id. */
export function workspaceDisplayName(opts = {}) {
  const id = opts.id || opts.workspace_id || null;
  const label = (opts.label || opts.workspace_label || "").trim();
  if (label) return label;
  const roots = Array.isArray(opts.roots) ? opts.roots : [];
  const folders = [];
  for (const r of roots) {
    const base = String(r || "").replace(/\/+$/, "").split("/").filter(Boolean).pop();
    if (base && !folders.includes(base)) folders.push(base);
  }
  if (!folders.length && Array.isArray(opts.agents)) {
    for (const a of opts.agents) {
      const base = String(a?.cwd || "").replace(/\/+$/, "").split("/").filter(Boolean).pop();
      if (base && !folders.includes(base)) folders.push(base);
    }
  }
  if (folders.length === 1) return folders[0];
  if (folders.length > 1) return folders.slice(0, 2).join("+");
  return id || "?";
}

/** Title with id for disambiguation: `novo (w5A)`. */
export function workspaceTitleWithId(opts = {}) {
  const id = opts.id || opts.workspace_id || "";
  const name = workspaceDisplayName(opts);
  if (id && name !== id) return `${name} (${id})`;
  return name || id || "?";
}

/**
 * Format a workspace-wide agent/pane roster for wake messages.
 * @param {Array<object>} agents agents in one workspace (from /push/state or hello)
 * @param {string|null} focusPane pane that triggered this wake (marked)
 * @param {{label?: string, id?: string, roots?: string[]}|null} workspaceMeta
 * @returns {{roster: string, idle_count: number, working_count: number, idle_hint: string, workspace_label: string}}
 */
export function formatWorkspaceRoster(agents, focusPane = null, workspaceMeta = null) {
  const list = Array.isArray(agents) ? agents.slice() : [];
  list.sort((a, b) => String(a?.pane || "").localeCompare(String(b?.pane || "")));
  const wsId = workspaceMeta?.id || list[0]?.workspace || null;
  const workspace_label = workspaceTitleWithId({
    id: wsId,
    label: workspaceMeta?.label,
    roots: workspaceMeta?.roots,
    agents: list,
  });
  let working_count = 0;
  let idle_count = 0;
  const lines = [];
  for (const a of list) {
    const status = a?.status || "unknown";
    if (status === "working") working_count += 1;
    else if (IDLE_LIKE.has(status)) idle_count += 1;
    const pane = a?.pane || "?";
    const agent = a?.name || a?.agent || "(unnamed)";
    const title = String(a?.terminal_title || a?.label || "").replace(/\s+/g, " ").trim().slice(0, 120);
    const cwd = String(a?.cwd || "").replace(/\/Users\/[^/]+/, "~").slice(-60);
    const mark = focusPane && pane === focusPane ? " ← focus" : "";
    const doing = title || "(no terminal title)";
    lines.push(`- ${pane} · ${agent} · ${status}${mark}\n  title/doing: ${doing}${cwd ? `\n  cwd: ${cwd}` : ""}`);
  }
  const roster = lines.length
    ? `workspace ${workspace_label} pane roster (${list.length}):\n${lines.join("\n")}`
    : `workspace ${workspace_label} pane roster: (no agents)`;
  const idle_hint = idle_count > 0
    ? `${idle_count} idle pane(s). Decide on the web: (1) keep for the next task (2) summarize / write docs and wrap up (3) reclaim and start a new agent. Do not default to closing or keeping all.`
    : "";
  return { roster, idle_count, working_count, idle_hint, workspace_label };
}

/** Render a wake template; agent/pane identify focus while workspace_label is the binding. */
export function buildWakeTemplate(template, fields) {
  const t = (template ?? "").trim();
  if (!t) return "";
  const output = String(fields.output ?? "").slice(0, 4000).trim();
  const roster = String(fields.roster ?? "").trim();
  const idle_hint = String(fields.idle_hint ?? "").trim();
  let out = t
    .replaceAll("{agent}", fields.agent ?? "")
    .replaceAll("{pane}", fields.pane ?? "")
    .replaceAll("{status}", fields.status ?? "")
    .replaceAll("{workspace}", fields.workspace ?? "")
    .replaceAll("{workspace_label}", fields.workspace_label ?? fields.workspace ?? "")
    .replaceAll("{working_count}", String(fields.working_count ?? ""))
    .replaceAll("{roster}", roster)
    .replaceAll("{idle_hint}", idle_hint)
    .replaceAll("{output}", output);
  out = out.replace(/[ \t]+\n/g, "\n").replace(/\n{3,}/g, "\n\n").trim();
  return out;
}

/**
 * Workspace-scoped wake decision.
 *
 * - hello: arm when any scoped agent is working; recover a completed round if previously armed
 * - working: always arm without waking
 * - settled: emit partial while peers remain working, or round when all are settled
 */
export function reconcileWorkspaceWakeKind(kind, workingCount) {
  const n = Number.isFinite(Number(workingCount)) ? Math.max(0, Number(workingCount)) : 0;
  if (kind === "partial" && n === 0) return "round";
  if (kind === "round" && n > 0) return "partial";
  return kind;
}

export function decideWorkspaceWake(prev, kind, data, scopeAgents) {
  const status = prev?.status ?? null;
  const lastSettle = prev?.lastSettle ?? null;
  const settled = (s) => SETTLED_STATUSES.includes(s);
  const workingN = scopeWorkingCount(scopeAgents);

  if (kind === "hello") {
    if (workingN > 0) {
      return { wake: false, kind: "none", status: "working", lastSettle, working_count: workingN };
    }
    const anySettled = (scopeAgents || []).find((a) => settled(a?.status));
    const seq = anySettled
      ? (anySettled.seq ?? `hello:${anySettled.status}`)
      : `hello:empty`;
    if (status === "working" && (!lastSettle || lastSettle.seq !== seq)) {
      return {
        wake: true, kind: "round",
        status: anySettled?.status ?? "idle",
        lastSettle: { seq, at: Date.now() },
        working_count: 0,
      };
    }
    return {
      wake: false, kind: "none",
      status: anySettled?.status ?? status ?? "idle",
      lastSettle,
      working_count: 0,
    };
  }

  if (kind === "working") {
    return { wake: false, kind: "none", status: "working", lastSettle, working_count: Math.max(1, workingN) };
  }

  if (kind === "settled") {
    if (!settled(data?.status)) {
      return { wake: false, kind: "none", status, lastSettle, working_count: workingN };
    }
    const seq = data.seq ?? `t:${data.at}:${data.pane || ""}`;
    if (seq != null && lastSettle?.seq === seq) {
      return { wake: false, kind: "none", status: data.status, lastSettle, working_count: workingN };
    }
    if (workingN > 0) {
      const wake = status === "working";
      return {
        wake, kind: wake ? "partial" : "none",
        status: "working",
        lastSettle: wake ? { seq, at: Date.now() } : lastSettle,
        working_count: workingN,
      };
    }
    const wake = status === "working";
    return {
      wake, kind: wake ? "round" : "none",
      status: data.status,
      lastSettle: { seq, at: Date.now() },
      working_count: 0,
    };
  }

  return { wake: false, kind: "none", status, lastSettle, working_count: workingN };
}


/**
 * Pure progress-tick decision for the background setInterval callback.
 *
 * Rules:
 *   - Non-positive or nonnumeric progressTickSec disables ticks.
 *   - Only working state can tick.
 *   - Tick after progressTickSec has elapsed since lastTickAt.
 *
 * @param {{status: string|null, lastTickAt: number|null}} prev
 * @param {number} now Date.now()
 * @param {{progressTickSec: number}} cfg
 * @returns {boolean}
 */
export function shouldProgressTick(prev, now, cfg) {
  const sec = Number(cfg?.progressTickSec);
  if (!Number.isFinite(sec) || sec <= 0) return false; // Disabled.
  if (!prev || prev.status !== "working") return false; // Not working.
  const lastTickAt = prev.lastTickAt ?? null;
  if (typeof lastTickAt !== "number") return false; // No baseline.
  return now - lastTickAt >= sec * 1000;
}

/**
 * Progress-summary fingerprint: remove spinner, clock, and whitespace noise.
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
 * Decide whether to send a progress message after a tick.
 *
 * Rules:
 *   - progressTickSec controls checks, not send cadence.
 *   - After a send, skip until progressFallbackSec has elapsed from lastSentAt.
 *   - Outside the cooldown, changed output yields new_output; unchanged output at
 *     the fallback boundary yields fallback.
 *
 * lastSentAt is initialized when armed and updated after each send.
 * hasProgressSent records whether this working round sent progress.
 *
 * @param {{lastSentAt: number|null, lastOutputSent: string, hasProgressSent?: boolean}} prev
 * @param {number} now
 * @param {string} output summary for this round
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

  // After the first send, enforce cooldown from the actual last send time.
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

/**
 * Fingerprint of text we injected as a continue-nudge.
 * Use this on assistantText (should never match) or to recognize our own user bubble —
 * never as a reason to skip judging the *next* assistant reply after we nudged.
 */
export function isIdleNudgeText(text) {
  const t = String(text || "").trim();
  if (!t) return false;
  // Legacy heuristic templates (short injected lines).
  if (t.length <= 400 && /tools\/call\s*=\s*0|MCP 本轮|talk-without-tools|禁止口头\s*PASS|停在半途|剩余项.*PASS\/FAIL/i.test(t)) {
    return true;
  }
  // Short continue lines we just injected from the judge model.
  // No \b after 继续: CJK is not a JS word char, so bare "继续" would miss.
  if (t.length <= 120 && (/^继续/u.test(t) || /^continue\b/i.test(t))) return true;
  return false;
}

/** True when composer text looks like an extension-injected herdr wake template. */
export function isHerdrWakeComposerText(text) {
  const t = String(text || "").replace(/\s+/g, " ").trim();
  if (!t) return false;
  return /^herdr workspace\b/i.test(t);
}

/**
 * Whether assistant text looks like a finished prose reply (not a mid-turn tool stub).
 * Used to avoid LLM nudge while ChatGPT is still invoking tools or showing short status lines.
 */
export function looksLikeSubstantiveReply(text) {
  const t = String(text || "").replace(/\s+/g, " ").trim();
  if (t.length < 60) return false;
  if (/^(?:calling|called|running|searching|fetching|executing|using|invoking|waiting)\b/i.test(t) && t.length < 160) {
    return false;
  }
  if (/^herdr_[a-z_]+\b/i.test(t) && t.length < 140) return false;
  const stripped = t.replace(/\{"tool"[^}]*\}/gi, "").trim();
  if (stripped.length < 50) return false;
  return true;
}

/** Stable short hash of assistant body so we do not re-judge identical settled text. */
export function assistantNudgeFingerprint(text) {
  const t = String(text || "").replace(/\s+/g, " ").trim().slice(0, 2000);
  let h = 2166136261;
  for (let i = 0; i < t.length; i++) {
    h ^= t.charCodeAt(i);
    h = Math.imul(h, 16777619);
  }
  return `${t.length}:${(h >>> 0).toString(16)}`;
}

/** Default judge prompt (shown pre-filled in Options). Placeholder: {content} */
export const LEGACY_DEFAULT_LLM_JUDGE_PROMPT =
  "判断下面这段对话里助手是否已经做完用户要求的全部任务。"
  + "若已全部做完，只回复：好的"
  + "若还有下一步/未完成/待验证，只回复：继续，按你的建议推进"
  + "不要解释、不要加其它字。对话：\n{content}";

export const DEFAULT_LLM_JUDGE_PROMPT =
  "判断下面这段对话里助手是否已经做完用户要求的全部任务。"
  + "若已全部做完，只回复：好的。"
  + "若还有下一步/未完成/待验证，只回复：继续，按你的建议推进。"
  + "不要解释、不要加其它字。对话：\n{content}";

/** Default no-send keywords (shown pre-filled in Options, one per line). */
export const DEFAULT_LLM_SKIP_KEYWORDS = ["好的", "done", "ok", "completed", "finished"];

export const DEFAULT_LLM_SKIP_KEYWORDS_TEXT = DEFAULT_LLM_SKIP_KEYWORDS.join("\n");

/**
 * Parse skip-keyword textarea: one per line, or comma / Chinese comma separated.
 * Empty input falls back to built-in list (safety net).
 * @param {string|string[]|null|undefined} raw
 * @returns {string[]}
 */
export function parseLlmSkipKeywords(raw) {
  if (Array.isArray(raw)) {
    const list = raw.map((s) => String(s || "").trim()).filter(Boolean);
    return list.length ? list : [...DEFAULT_LLM_SKIP_KEYWORDS];
  }
  const t = String(raw || "").trim();
  if (!t) return [...DEFAULT_LLM_SKIP_KEYWORDS];
  const list = t
    .split(/[\n,，;；]+/)
    .map((s) => s.trim())
    .filter(Boolean);
  return list.length ? list : [...DEFAULT_LLM_SKIP_KEYWORDS];
}

/**
 * Whether a judge reply matches a no-send (done) keyword.
 * Whole-reply match after stripping trailing punctuation; ASCII case-insensitive.
 */
export function llmReplyMatchesSkipKeyword(reply, keywords) {
  const t = String(reply || "").trim()
    .replace(/^["「『]+|["」』]+$/g, "")
    .replace(/^```\w*\n?|\n?```$/g, "")
    .trim();
  if (!t) return false;
  const core = t.replace(/[.!！。…\s]+$/u, "").trim();
  const list = parseLlmSkipKeywords(keywords);
  for (const kw of list) {
    const k = String(kw).trim();
    if (!k) continue;
    if (/^[a-z0-9_-]+$/i.test(k)) {
      if (core.toLowerCase() === k.toLowerCase()) return true;
      if (new RegExp(`^${k.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}[.!！。…]*$`, "i").test(t)) return true;
    } else if (core === k || new RegExp(`^${k.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}[.!！。…]*$`, "u").test(t)) {
      return true;
    }
  }
  return false;
}

/**
 * Whether LLM post-turn judge is configured (all three required; template may be empty → built-in).
 * @param {{ llmJudgeBaseUrl?: string, llmJudgeApiKey?: string, llmJudgeModel?: string }} cfg
 */
export function isLlmJudgeConfigured(cfg) {
  return Boolean(
    String(cfg?.llmJudgeBaseUrl || "").trim()
    && String(cfg?.llmJudgeApiKey || "").trim()
    && String(cfg?.llmJudgeModel || "").trim(),
  );
}

/**
 * Build OpenAI-compatible chat/completions URL from a base like
 * https://host/v1 or https://host/v1/chat/completions
 */
export function llmJudgeCompletionsUrl(baseUrl) {
  const b = String(baseUrl || "").trim().replace(/\/+$/, "");
  if (!b) return "";
  if (/\/chat\/completions$/i.test(b)) return b;
  if (/\/v1$/i.test(b)) return `${b}/chat/completions`;
  return `${b}/v1/chat/completions`;
}

/**
 * Fill {content} (and optional {user}/{assistant}) into the judge prompt.
 */
export function buildLlmJudgeUserMessage(template, parts) {
  const tpl = String(template || "").trim() || DEFAULT_LLM_JUDGE_PROMPT;
  const user = String(parts?.userText || "").trim().slice(0, 1500);
  const assistant = String(parts?.assistantText || "").trim().slice(0, 6000);
  const content = user
    ? `用户:\n${user}\n\n助手:\n${assistant}`
    : assistant;
  return tpl
    .replaceAll("{content}", content)
    .replaceAll("{user}", user)
    .replaceAll("{assistant}", assistant);
}

/**
 * Strong self-declared pending-work signals in the assistant's own tail.
 * Keep this deliberately narrower than generic "next steps" wording so a
 * completed answer that merely offers optional suggestions is not auto-woken.
 */
export function assistantDeclaresPendingWork(text) {
  const tail = String(text || "").trim().slice(-2400);
  if (!tail) return false;
  return /(?:^|\n)\s*(?:下一步|接下来)\s*[:：]?\s*(?:\n\s*)?(?:我会|我将|继续|先(?:查|检查|读取|确认|修复|处理|等待)|需要(?:查|检查|读取|确认|修复|处理|等待))/mu.test(tail)
    || /(?:^|[。！？\n])\s*我(?:会|将)(?:继续|接着|再)(?:做|查|读取|检查|确认|修复|推进|处理|等待)?/u.test(tail)
    || /(?:^|[。！？\n])\s*(?:仍需|还需要|尚需|尚未完成|仍未完成|待(?:验证|确认|检查|处理|完成))(?:[：:，,。.!！\s]|$)/u.test(tail)
    || /(?:^|\n)\s*(?:next|remaining work)\s*[:\-]?\s*(?:i will|i'll|continue|need to)/im.test(tail)
    || /\bI\s+(?:will|need to)\s+continue\b/i.test(tail)
    || /\b(?:still need to|not yet complete|pending (?:verification|validation|work|check))\b/i.test(tail);
}

/**
 * Interpret a tiny judge-model reply.
 * - Matches no-send keywords → done (do not wake)
 * - Looks like continue / unfinished → wake with the model reply text itself
 *
 * @param {string} text
 * @param {{ skipKeywords?: string|string[] }|string|null} [opts]
 * @returns {{ done: boolean, cont: boolean, nudgeText: string, raw: string }}
 */
export function interpretLlmJudgeReply(text, opts = null) {
  const options = typeof opts === "string" || opts == null
    ? { skipKeywords: typeof opts === "string" ? undefined : undefined }
    : opts;
  // Legacy: second arg was continueFallback string — ignore it; always use model text.
  const skipRaw = options && typeof options === "object" ? options.skipKeywords : undefined;
  const raw = String(text || "").trim();
  if (!raw) return { done: false, cont: false, nudgeText: "", raw };
  const t = raw.replace(/^["「『]+|["」』]+$/g, "").replace(/^```\w*\n?|\n?```$/g, "").trim();
  const nudgeText = t.slice(0, 500);

  const wantsContinue = /继续|推进|没(有)?做完|未完成|下一步|还没/u.test(t) || /^continue\b/i.test(t);
  // Mixed "好的，没有完成。继续。" → continue with model text.
  if (wantsContinue && /继续|推进|没(有)?做完|未完成/.test(t) && !llmReplyMatchesSkipKeyword(t, skipRaw)) {
    return { done: false, cont: true, nudgeText, raw: t };
  }
  if (wantsContinue && /继续/.test(t) && /好的|没(有)?做完|未完成/.test(t)) {
    return { done: false, cont: true, nudgeText, raw: t };
  }
  if (llmReplyMatchesSkipKeyword(t, skipRaw)) {
    return { done: true, cont: false, nudgeText: "", raw: t };
  }
  if (wantsContinue) {
    return { done: false, cont: true, nudgeText, raw: t };
  }
  return { done: false, cont: false, nudgeText: "", raw: t };
}

/** Popup pace presets removed — use numeric seconds in popup/options (0 = off). */
