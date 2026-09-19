// Jev semantic gate — pure request/response helpers.
// Provider transport lives in background.js. Users configure only endpoint/model/key.

export const DEFAULT_JEV_BASE_URL = "https://api.typesafe.ai/v1";
export const DEFAULT_JEV_MODEL = "jev-latest";
export const DEFAULT_JEV_THRESHOLD = 0.70;

export function normalizeJevJudgeThreshold(value) {
  const n = Number(value);
  if (!Number.isFinite(n) || n < 0.5 || n > 0.99) return DEFAULT_JEV_THRESHOLD;
  return n;
}

export function isJevJudgeConfigured(cfg) {
  return Boolean(String(cfg?.jevJudgeBaseUrl || "").trim())
    && Boolean(String(cfg?.jevJudgeApiKey || "").trim())
    && Boolean(String(cfg?.jevJudgeModel || "").trim());
}

export function jevSystemOneUrl(rawBaseUrl) {
  const raw = String(rawBaseUrl || "").trim();
  if (!raw) return "";
  const parsed = new URL(raw);
  if (parsed.protocol !== "http:" && parsed.protocol !== "https:") throw new Error("invalid_url");
  const path = parsed.pathname.replace(/\/+$/, "");
  parsed.pathname = /\/systemone$/i.test(path) ? path : `${path}/systemone`;
  parsed.search = "";
  parsed.hash = "";
  return parsed.toString();
}

function boundedText(value, maxChars) {
  const text = String(value || "").trim();
  if (text.length <= maxChars) return text;
  return text.slice(-maxChars);
}

export function buildJevPendingWorkRequest(userText, assistantText, model = DEFAULT_JEV_MODEL) {
  return {
    state: {
      user_request: boundedText(userText, 6000),
      assistant_latest_response: boundedText(assistantText, 12000),
    },
    model: String(model || DEFAULT_JEV_MODEL).trim() || DEFAULT_JEV_MODEL,
    questions: {
      has_unfinished_work: {
        type: "noul",
        instructions: "Does the assistant's latest response leave concrete work from the user's request unfinished that the assistant itself can continue in another turn?",
        criteria: {
          true: "Concrete requested work remains and the assistant can keep working without a new user decision, for example implementation, investigation, verification, cleanup, or a next step it explicitly committed to do.",
          false: "The requested work is complete, the response only offers optional future help, or the remaining action requires a user/external decision or input before the assistant can continue.",
        },
      },
    },
  };
}

export const JEV_GOAL_SIGNAL_KEYS = Object.freeze([
  "can_continue",
  "needs_human",
  "waiting_external",
  "task_completed",
  "needs_handoff",
]);

export function buildJevGoalSemanticRequest({
  objective = "",
  userText = "",
  assistantText = "",
  openTodos = [],
  boundary = "",
  runtimeSummary = "",
} = {}, model = DEFAULT_JEV_MODEL) {
  return {
    state: {
      objective: boundedText(objective, 4000),
      user_request: boundedText(userText, 4000),
      assistant_latest_response: boundedText(assistantText, 8000),
      open_todos: Array.isArray(openTodos)
        ? openTodos.slice(0, 16).map((v) => boundedText(v, 240))
        : [],
      boundary: boundedText(boundary, 80),
      runtime_summary: boundedText(runtimeSummary, 1200),
    },
    model: String(model || DEFAULT_JEV_MODEL).trim() || DEFAULT_JEV_MODEL,
    questions: {
      can_continue: {
        type: "noul",
        instructions: "Can the assistant autonomously continue useful work toward the current objective right now without a new human decision or unavailable external event?",
      },
      needs_human: {
        type: "noul",
        instructions: "Does the next safe step require a human decision, approval, subjective preference, credential, payment, publication, deletion, or other human-owned action?",
      },
      waiting_external: {
        type: "noul",
        instructions: "Is progress currently blocked waiting for an external system, running agent, CI/job, provider, or other event that the assistant should not replace with another Continue message?",
      },
      task_completed: {
        type: "noul",
        instructions: "Does the latest state appear to have completed the requested objective with no concrete work remaining? This is only a semantic hint, not evidence that TODOs are actually closed.",
      },
      needs_handoff: {
        type: "noul",
        instructions: "Does the latest state indicate that continuing the same objective requires handing off to a fresh conversation because of context pressure or an explicit planned continuity transfer?",
      },
    },
  };
}

export function interpretJevGoalSemanticAnswer(payload, threshold = DEFAULT_JEV_THRESHOLD) {
  const t = normalizeJevJudgeThreshold(threshold);
  const probabilities = {};
  for (const key of JEV_GOAL_SIGNAL_KEYS) {
    const p = Number(payload?.answers?.[key]?.noul);
    if (!Number.isFinite(p) || p < 0 || p > 1) {
      return { ok: false, reason: "bad_response", probabilities: null, strong: [] };
    }
    probabilities[key] = p;
  }
  return {
    ok: true,
    threshold: t,
    probabilities,
    strong: JEV_GOAL_SIGNAL_KEYS.filter((key) => probabilities[key] >= t),
    model: typeof payload?.model === "string" ? payload.model : null,
    usage: payload?.usage && typeof payload.usage === "object" ? payload.usage : null,
  };
}

export function interpretJevPendingWorkAnswer(payload, threshold = DEFAULT_JEV_THRESHOLD) {
  const p = Number(payload?.answers?.has_unfinished_work?.noul);
  if (!Number.isFinite(p) || p < 0 || p > 1) {
    return { ok: false, reason: "bad_response", probability: null, signal: "unknown" };
  }
  const t = normalizeJevJudgeThreshold(threshold);
  return {
    ok: true,
    probability: p,
    threshold: t,
    signal: p >= t ? "continue" : p <= 1 - t ? "done" : "uncertain",
    model: typeof payload?.model === "string" ? payload.model : null,
    usage: payload?.usage && typeof payload.usage === "object" ? payload.usage : null,
  };
}

export function decideJevAutoPolicy(jev) {
  if (jev?.ok) {
    if (jev.signal === "continue") return { action: "continue", reason: "jev_continue" };
    if (jev.signal === "done") return { action: "stop", reason: "jev_done" };
    return { action: "fallback", reason: "jev_uncertain" };
  }
  return { action: "fallback", reason: "jev_unavailable" };
}
