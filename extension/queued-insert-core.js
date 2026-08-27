// queued-insert-core.js — pure queue semantics for non-interrupting browser messages.
// Browser/storage transport lives in background.js; this module stays deterministic.

export const QUEUED_INSERT_STORAGE_KEY = "h2wQueuedInsertV1";
export const QUEUED_INSERT_MAX_ITEMS = 20;
export const QUEUED_INSERT_MAX_CHARS = 24000;
export const QUEUED_INSERT_MAX_AGE_MS = 7 * 24 * 60 * 60 * 1000;

function cleanText(value) {
  return String(value ?? "").replace(/\r\n?/g, "\n").trim();
}

function cleanConvKey(value) {
  return String(value ?? "").trim();
}

function stableHash(value) {
  let hash = 2166136261;
  for (const ch of String(value ?? "")) {
    hash ^= ch.codePointAt(0);
    hash = Math.imul(hash, 16777619);
  }
  return (hash >>> 0).toString(16);
}

function normalizeEntry(row, now) {
  if (!row || typeof row !== "object") return null;
  const text = cleanText(row.text);
  const id = String(row.id ?? "").trim();
  const createdAt = Number(row.created_at);
  if (!text || !id || !Number.isFinite(createdAt)) return null;
  if (createdAt > now + 60_000) return null;
  if (now - createdAt > QUEUED_INSERT_MAX_AGE_MS) return null;
  return { id, text, created_at: createdAt };
}

export function normalizeQueuedInsertState(raw, now = Date.now()) {
  const source = raw && typeof raw === "object" ? raw : {};
  const conversations = source.conversations && typeof source.conversations === "object"
    ? source.conversations
    : {};
  const next = { version: 1, conversations: {} };
  for (const [convKeyRaw, rows] of Object.entries(conversations)) {
    const convKey = cleanConvKey(convKeyRaw);
    if (!convKey || !Array.isArray(rows)) continue;
    const seen = new Set();
    const kept = [];
    let chars = 0;
    for (const row of rows) {
      const entry = normalizeEntry(row, now);
      if (!entry || seen.has(entry.id)) continue;
      if (kept.length >= QUEUED_INSERT_MAX_ITEMS) break;
      if (chars + entry.text.length > QUEUED_INSERT_MAX_CHARS) break;
      seen.add(entry.id);
      kept.push(entry);
      chars += entry.text.length;
    }
    if (kept.length) next.conversations[convKey] = kept;
  }
  return next;
}

export function queuedInsertStatus(state, convKeyRaw) {
  const convKey = cleanConvKey(convKeyRaw);
  const rows = normalizeQueuedInsertState(state).conversations[convKey] || [];
  return {
    count: rows.length,
    chars: rows.reduce((sum, row) => sum + row.text.length, 0),
    oldest_at: rows[0]?.created_at ?? null,
  };
}

export function enqueueQueuedInsert(state, convKeyRaw, textRaw, options = {}) {
  const convKey = cleanConvKey(convKeyRaw);
  const text = cleanText(textRaw);
  const now = Number.isFinite(Number(options.now)) ? Number(options.now) : Date.now();
  if (!convKey) return { ok: false, error: "conversation-required", state: normalizeQueuedInsertState(state, now) };
  if (!text) return { ok: false, error: "message-required", state: normalizeQueuedInsertState(state, now) };

  const next = normalizeQueuedInsertState(state, now);
  const rows = [...(next.conversations[convKey] || [])];
  const chars = rows.reduce((sum, row) => sum + row.text.length, 0);
  if (rows.length >= QUEUED_INSERT_MAX_ITEMS || chars + text.length > QUEUED_INSERT_MAX_CHARS) {
    return { ok: false, error: "queue-full", state: next, status: queuedInsertStatus(next, convKey) };
  }
  const id = String(options.id || `qi:${now.toString(36)}:${stableHash(`${convKey}\n${text}\n${rows.length}`)}`);
  const entry = { id, text, created_at: now };
  rows.push(entry);
  next.conversations[convKey] = rows;
  return { ok: true, state: next, entry, status: queuedInsertStatus(next, convKey) };
}

export function queuedInsertBatch(state, convKeyRaw) {
  const convKey = cleanConvKey(convKeyRaw);
  const next = normalizeQueuedInsertState(state);
  const rows = next.conversations[convKey] || [];
  if (!rows.length) return null;
  const entryIds = rows.map((row) => row.id);
  const text = rows.map((row) => row.text).join("\n\n");
  return {
    batch_id: `qib:${stableHash(`${convKey}\n${entryIds.join("\n")}`)}`,
    entry_ids: entryIds,
    text,
    count: rows.length,
    chars: text.length,
  };
}

export function ackQueuedInsertBatch(state, convKeyRaw, entryIds = []) {
  const convKey = cleanConvKey(convKeyRaw);
  const next = normalizeQueuedInsertState(state);
  const remove = new Set((entryIds || []).map((id) => String(id)));
  if (!convKey || !remove.size) return next;
  const rows = (next.conversations[convKey] || []).filter((row) => !remove.has(row.id));
  if (rows.length) next.conversations[convKey] = rows;
  else delete next.conversations[convKey];
  return next;
}

export function clearQueuedInserts(state, convKeyRaw) {
  const convKey = cleanConvKey(convKeyRaw);
  const next = normalizeQueuedInsertState(state);
  if (convKey) delete next.conversations[convKey];
  return next;
}

export function moveQueuedInserts(state, sourceConvKeyRaw, targetConvKeyRaw) {
  const sourceConvKey = cleanConvKey(sourceConvKeyRaw);
  const targetConvKey = cleanConvKey(targetConvKeyRaw);
  const next = normalizeQueuedInsertState(state);
  if (!sourceConvKey || !targetConvKey || sourceConvKey === targetConvKey) {
    return { ok: false, error: "conversation-invalid", state: next, moved_count: 0 };
  }
  const source = next.conversations[sourceConvKey] || [];
  if (!source.length) return { ok: true, state: next, moved_count: 0 };
  const target = next.conversations[targetConvKey] || [];
  const seen = new Set();
  const merged = [...source, ...target]
    .sort((a, b) => a.created_at - b.created_at)
    .filter((row) => {
      if (seen.has(row.id)) return false;
      seen.add(row.id);
      return true;
    });
  const chars = merged.reduce((sum, row) => sum + row.text.length, 0);
  if (merged.length > QUEUED_INSERT_MAX_ITEMS || chars > QUEUED_INSERT_MAX_CHARS) {
    return { ok: false, error: "target-queue-capacity", state: next, moved_count: 0 };
  }
  next.conversations[targetConvKey] = merged;
  delete next.conversations[sourceConvKey];
  return { ok: true, state: next, moved_count: source.length };
}
