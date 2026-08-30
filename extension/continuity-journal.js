// continuity-journal.js — bounded retry/cache helpers for the durable continuity
// journal. Rust state_store v5 is AUTHORITATIVE: resumes and durable truth come
// from `POST /extension/continuity/turn` (ack) and the `continuity.*` herdr_call
// methods. This module holds only deterministic helpers the extension uses to
// (a) derive stable idempotent message ids / fingerprints for retry/cache and
// (b) build the compact continuity-reference seed. Nothing here is an MCP tool;
// the public epoch-2 18-tool contract is unchanged.

export const CONTINUITY_JOURNAL_STORAGE_KEY = "herdrContinuityJournalV1";
export const CONTINUITY_SEED_PREFIX = "[HERDR_CONTINUITY_REF";
export const CONTINUITY_SEED_END = "[END_HERDR_CONTINUITY_REF]";

/** FNV-1a deterministic hash, mirroring the queued-insert stableHash. */
export function stableHash(value) {
  let hash = 2166136261;
  for (const ch of String(value ?? "")) {
    hash ^= ch.codePointAt(0);
    hash = Math.imul(hash, 16777619);
  }
  return (hash >>> 0).toString(16);
}

/** Deterministic fingerprint for one finalized user->assistant turn. */
export function turnFingerprint({ convKey, startedAt, userText, assistantText } = {}) {
  return stableHash([
    String(convKey || ""),
    String(startedAt || ""),
    String(userText || "").trim().slice(-4000),
    String(assistantText || "").trim().slice(-8000),
  ].join("\u0001"));
}

/**
 * Deterministic message id for one finalized side. Rust dedupes replays with
 * `PRIMARY KEY (continuity_id, message_id)`, so this must be stable for the same
 * logical turn and distinct between user/assistant sides of that turn. A
 * page-provided message id is preferred and passed through verbatim.
 */
export function continuityMessageId({ messageId, convKey, role, text, startedAt } = {}) {
  const supplied = String(messageId || "").trim();
  if (supplied) return supplied;
  const turnRole = String(role || "").toLowerCase();
  const body = String(text || "").trim();
  return `jt:${turnFingerprint({
    convKey,
    startedAt,
    userText: turnRole === "user" ? body : "",
    assistantText: turnRole === "assistant" ? body : "",
  }).slice(0, 16)}`;
}

/**
 * Compact seed reference the target conversation carries in place of the large
 * model-written HERDR_HANDOFF_V1 when Rust has acknowledged durable state. The
 * target model MUST call `herdr_call(method="continuity.resume", ...)` to fetch
 * the authoritative journal, then re-check live Herdr/runtime/Git state before
 * any mutation. HERDR_HANDOFF_V1 / source-transcript remains the fallback.
 */
export function buildContinuitySeed({ transferId, continuityId } = {}) {
  const tid = String(transferId || "").trim();
  const cid = String(continuityId || "").trim();
  if (!tid || !cid) throw new Error("transferId and continuityId are required");
  return [
    "继续同一个项目。以下内容由 Herdr 浏览器扩展从上一段对话接力（durable continuity journal）。",
    "continuity_id 是稳定的工作状态链标识，不是实时状态未变化的证明。",
    "接手后第一步：调用 herdr_call，method 为 continuity.resume，params 为 {\"continuity_id\":\"<此连续链>\"}，读取 Rust 持久化的权威 journal（目标、已完成、决定、约束、anchors）。",
    "continuity.resume 之后：重新检查相关 Herdr/runtime/Git 实时状态，再决定是否开始任何 mutation。",
    "不要因为 journal 或 handoff 提到过某件事，就重复执行已经完成的工作。",
    "",
    `${CONTINUITY_SEED_PREFIX} id=${tid} continuity_id=${cid}]`,
    `continuity_id: ${cid}`,
    CONTINUITY_SEED_END,
  ].join("\n");
}

export function continuitySeedContainsReference(text, transferId) {
  const id = String(transferId || "").trim();
  if (!id) return false;
  return String(text || "").includes(`${CONTINUITY_SEED_PREFIX} id=${id}`);
}