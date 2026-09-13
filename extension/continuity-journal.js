// continuity-journal.js — bounded retry/cache helpers for the durable continuity
// journal. Rust state_store v5 is AUTHORITATIVE: resumes and durable truth come
// from `POST /extension/continuity/turn` (ack) and the `continuity.*` herdr_call
// methods. This module holds only deterministic helpers the extension uses to
// (a) derive stable idempotent message ids / fingerprints for retry/cache and
// (b) build the compact continuity-reference seed. Nothing here is an MCP tool;
// the public epoch-2 18-tool contract is unchanged.

export const CONTINUITY_JOURNAL_STORAGE_KEY = "herdrContinuityJournalV1";

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
 * any mutation. New ChatGPT handoffs carry the source URL and use only this durable reference path. HERDR_HANDOFF_V1 remains read-only compatibility for already-existing legacy transfers and provider-specific legacy contracts.
 */