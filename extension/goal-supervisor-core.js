// goal-supervisor-core.js — deterministic goal-aware supervisor for Auto.
//
// AUTHORITY
// ---------
// Work Chain / Planner Lease / Execution Lane / Continuity / Work Memory remain
// the only state authorities. This module owns no store, no schema, no
// scheduler and no planning authority. The Goal Ledger is a *derived*
// projection: either the goal section an existing Work Memory checkpoint blob
// already carries (work_memory.checkpoint.put / work_memory.resume pass
// caller-supplied JSON through unchanged), or a bounded projection of the
// continuity-anchored authored turns. It is recomputed at each boundary and is
// never persisted by this module.
//
// SCOPE
// -----
// Pure policy only: no chrome.*, no fetch, no timers, no provider/model/key.
// It provides the fixed decision vocabulary, the bounded model input, the strict
// decision contract, the deterministic pre-execution guard, a transport-injected
// bounded adapter, and one orchestration entrypoint.

export const SUPERVISOR_VERSION = 1;

/** The complete decision vocabulary. Nothing outside this list is executable. */
export const SUPERVISOR_DECISIONS = Object.freeze([
  "COMPLETE", "WAIT_EXTERNAL", "CONTINUE", "ANSWER_DECISION", "RECOVER", "HANDOFF", "ASK_HUMAN",
]);

export const TODO_STATUSES = Object.freeze(["pending", "working", "blocked", "done", "superseded"]);
export const RETRY_CLASSES = Object.freeze(["none", "transient", "provider", "browser", "uncertain"]);

/** Boundaries where one supervisor decision may be requested — nothing else. */
export const SUPERVISOR_BOUNDARIES = Object.freeze([
  "turn_settled", "agent_meaningful_change", "silence_timeout",
  "provider_error", "browser_loss", "context_threshold", "handoff_settled",
]);

export const EVIDENCE_KINDS = Object.freeze([
  "herdr_agent", "herdr_exec", "herdr_test", "herdr_deploy", "browser_result", "human",
]);

/** Only these can close a TODO. `browser_result` proves a turn settled, not that work landed. */
const HERDR_EVIDENCE_KINDS = Object.freeze(["herdr_agent", "herdr_exec", "herdr_test", "herdr_deploy"]);

/** Human-owned TODOs may close with human evidence; everything else needs Herdr evidence. */
export const HUMAN_OWNED_TODO_KIND = "human";

export const SUPERVISOR_LIMITS = Object.freeze({
  objective: 400,
  constraint: 200,
  constraints: 8,
  todos: 12,
  todoTitle: 160,
  evidencePerTodo: 4,
  evidenceRef: 200,
  evidenceSummary: 240,
  acceptedDecisions: 8,
  acceptedDecisionText: 240,
  authoredTurns: 6,
  turnChars: 600,
  evidenceSummaries: 12,
  reason: 300,
  message: 1200,
  totalInputChars: 6000,
});

export const DEFAULT_SUPERVISOR_POLICY = Object.freeze({
  continueBudget: 6,
  recoverBudget: 2,
  /** Bound on supervisor model invocations for one boundary. */
  maxDecisionAttempts: 2,
  /** Minimum spacing between two supervisor runs for one conversation. */
  minBoundaryIntervalMs: 15000,
  /** Context states at or above which HANDOFF is inside policy. */
  handoffStates: Object.freeze([
    "handoff_prepare", "rollover_recommended", "rollover_required", "high_risk",
  ]),
});

/** Section name inside the EXISTING Work Memory checkpoint JSON. Not a new schema. */
export const GOAL_CHECKPOINT_SECTION = "goal_supervisor";

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

function txt(value, max) {
  const s = String(value ?? "").replace(/\s+/g, " ").trim();
  return max > 0 ? s.slice(0, max) : s;
}

function raw(value, max) {
  const s = String(value ?? "").trim();
  return max > 0 ? s.slice(0, max) : s;
}

function posInt(value, fallback = 0) {
  const n = Number(value);
  return Number.isFinite(n) && n > 0 ? Math.floor(n) : fallback;
}

function oneOf(value, allowed, fallback = null) {
  const s = String(value ?? "").trim();
  return allowed.includes(s) ? s : fallback;
}

function list(value, limit) {
  return Array.isArray(value) ? value.slice(0, limit) : [];
}

/** FNV-1a, matching the other extension cores' deterministic hashing. */
export function supervisorHash(value) {
  let hash = 2166136261;
  for (const ch of String(value ?? "")) {
    hash ^= ch.codePointAt(0);
    hash = Math.imul(hash, 16777619);
  }
  return (hash >>> 0).toString(16);
}

// ---------------------------------------------------------------------------
// 1. derived Goal Ledger
// ---------------------------------------------------------------------------

export function emptyRetryBudget(policy = DEFAULT_SUPERVISOR_POLICY) {
  return {
    continue: { used: 0, max: posInt(policy.continueBudget, 6) },
    recover: { used: 0, max: posInt(policy.recoverBudget, 2) },
  };
}

function normalizeTodo(todo, index) {
  const t = todo && typeof todo === "object" ? todo : {};
  const status = oneOf(t.status, TODO_STATUSES, "pending");
  return {
    id: txt(t.id, 64) || `t${index + 1}`,
    title: txt(t.title, SUPERVISOR_LIMITS.todoTitle),
    kind: txt(t.kind, 24) || "unknown",
    status,
    owner: txt(t.owner, 40) || (status === "working" ? "webchat" : "unknown"),
    evidence: list(t.evidence, SUPERVISOR_LIMITS.evidencePerTodo).map((item) => {
      const e = item && typeof item === "object" ? item : {};
      return {
        kind: oneOf(e.kind, EVIDENCE_KINDS, null),
        ref: txt(e.ref, SUPERVISOR_LIMITS.evidenceRef),
        summary: txt(e.summary, SUPERVISOR_LIMITS.evidenceSummary),
        at: posInt(e.at, 0) || null,
      };
    }).filter((e) => Boolean(e.kind && e.ref)),
    superseded_reason: txt(t.superseded_reason, 120) || null,
    updated_at: posInt(t.updated_at, 0) || null,
  };
}

/**
 * Coerce arbitrary derived input into the bounded ledger shape. Unknown fields
 * are dropped, so a malformed projection can never smuggle new authority in.
 */
export function normalizeGoalLedger(input) {
  const raw0 = input && typeof input === "object" ? input : {};
  const L = SUPERVISOR_LIMITS;

  const seen = new Set();
  const todos = list(raw0.todos, L.todos)
    .map(normalizeTodo)
    .filter((todo) => Boolean(todo.title || todo.id))
    .filter((todo) => (seen.has(todo.id) ? false : (seen.add(todo.id), true)));

  const accepted = list(raw0.accepted_decisions, L.acceptedDecisions).map((item) => {
    const d = item && typeof item === "object" ? item : {};
    const id = txt(d.id, 64);
    const decision = txt(d.decision, L.acceptedDecisionText);
    if (!id || !decision) return null;
    return {
      id,
      decision,
      rationale: txt(d.rationale, L.acceptedDecisionText),
      derived_from: list(d.derived_from, 4).map((v) => txt(v, 64)).filter(Boolean),
      at: posInt(d.at, 0) || null,
    };
  }).filter(Boolean);

  const budget = emptyRetryBudget();
  budget.continue.used = posInt(raw0?.retry?.continue?.used, 0);
  budget.continue.max = posInt(raw0?.retry?.continue?.max, budget.continue.max);
  budget.recover.used = posInt(raw0?.retry?.recover?.used, 0);
  budget.recover.max = posInt(raw0?.retry?.recover?.max, budget.recover.max);

  const blocker = raw0.current_blocker && typeof raw0.current_blocker === "object"
    ? {
      kind: txt(raw0.current_blocker.kind, 40) || "unknown",
      detail: txt(raw0.current_blocker.detail, 240),
      since: posInt(raw0.current_blocker.since, 0) || null,
    }
    : null;

  const last = raw0.last_decision && typeof raw0.last_decision === "object" ? raw0.last_decision : null;

  return {
    version: SUPERVISOR_VERSION,
    derived: true,
    source: oneOf(raw0.source, ["work_memory_checkpoint", "continuity_turns"], "continuity_turns"),
    continuity_id: txt(raw0.continuity_id, 160) || null,
    work_chain_id: txt(raw0.work_chain_id, 160) || null,
    objective: txt(raw0.objective, L.objective),
    human_constraints: list(raw0.human_constraints, L.constraints).map((c) => txt(c, L.constraint)).filter(Boolean),
    accepted_decisions: accepted,
    todos,
    current_blocker: blocker,
    retry: budget,
    last_decision: last
      ? {
        decision: oneOf(last.decision, SUPERVISOR_DECISIONS, null),
        reason: txt(last.reason, L.reason),
        retry_class: oneOf(last.retry_class, RETRY_CLASSES, "none"),
        at: posInt(last.at, 0) || null,
      }
      : null,
    supervisor: {
      external_owner: txt(raw0?.supervisor?.external_owner, 60) || null,
      waiting_since: posInt(raw0?.supervisor?.waiting_since, 0) || null,
      last_boundary: oneOf(raw0?.supervisor?.last_boundary, SUPERVISOR_BOUNDARIES, null),
      last_run_at: posInt(raw0?.supervisor?.last_run_at, 0) || null,
      last_input_fingerprint: txt(raw0?.supervisor?.last_input_fingerprint, 32) || null,
    },
    closed: raw0.closed === true,
    updated_at: posInt(raw0.updated_at, 0) || Date.now(),
  };
}

/**
 * Read the goal section an existing Work Memory checkpoint blob already
 * carries. Returns null when the checkpoint has no goal section, so the caller
 * falls back to deriving from the continuity-anchored turns instead of
 * inventing an objective.
 */
export function goalLedgerFromCheckpoint(checkpoint) {
  if (!checkpoint) return null;
  const section = checkpoint?.[GOAL_CHECKPOINT_SECTION];
  if (!section || typeof section !== "object") return null;
  const ledger = normalizeGoalLedger({ ...section, source: "work_memory_checkpoint" });
  return ledger.objective || ledger.todos.length ? ledger : null;
}

/** Reject a Work Memory locator that is not the exact, unique match for a conversation. */
export function resolveAuthoritativeWorkMemoryLocator(candidate, expectedContinuityId) {
  const locator = candidate?.work_memory && typeof candidate.work_memory === "object"
    ? candidate.work_memory
    : null;
  if (!locator) return { ok: false, reason: "no_work_memory_locator" };
  const projectRef = String(locator.project_ref || "").trim();
  const repoId = String(locator.repo_id || "").trim();
  const workChainId = String(locator.work_chain_id || "").trim();
  if (!projectRef || !repoId || !workChainId) {
    return { ok: false, reason: "incomplete_work_memory_locator" };
  }
  if (expectedContinuityId && String(candidate.continuity_id || "").trim() !== expectedContinuityId) {
    return { ok: false, reason: "locator_continuity_mismatch" };
  }
  return {
    ok: true,
    locator: { project_ref: projectRef, repo_id: repoId, work_chain_id: workChainId },
    continuity_id: String(candidate.continuity_id || "").trim() || null,
  };
}

/**
 * Build the Work Memory checkpoint JSON carrying the derived goal, preserving
 * the other fields already present in the authoritative checkpoint blob so this
 * consumer never clobbers a co-existing writer.
 */
export function mergeGoalCheckpoint(existingCheckpoint, ledger) {
  const base = existingCheckpoint && typeof existingCheckpoint === "object" && !Array.isArray(existingCheckpoint)
    ? { ...existingCheckpoint }
    : {};
  base[GOAL_CHECKPOINT_SECTION] = {
    version: SUPERVISOR_VERSION,
    objective: ledger.objective,
    human_constraints: ledger.human_constraints,
    accepted_decisions: ledger.accepted_decisions,
    todos: ledger.todos,
    current_blocker: ledger.current_blocker,
    retry: ledger.retry,
    last_decision: ledger.last_decision,
    supervisor: ledger.supervisor,
    closed: ledger.closed,
    updated_at: ledger.updated_at,
  };
  return base;
}

/**
 * CAS helper: given the error from work_memory.checkpoint.put, decide whether
 * it may be reconciled by re-reading (an expected-revision conflict) or must
 * fail closed (anything else). Last-write-wins is never allowed.
 */
export function classifyCheckpointPutError(error) {
  const message = String(error || "");
  const conflict = message.match(/work_memory_checkpoint_revision_conflict:(\d+)/);
  if (conflict) {
    return { kind: "revision_conflict", revision: Number(conflict[1]), retry: true };
  }
  if (/work_memory_not_found/.test(message)) return { kind: "not_found", retry: false };
  if (/work_memory_checkpoint_turn_not_found/.test(message)) return { kind: "turn_missing", retry: false };
  if (/work_memory_checkpoint_evidence_not_found/.test(message)) return { kind: "evidence_missing", retry: false };
  return { kind: "other", retry: false };
}

/**
 * Bounded projection of the continuity-anchored authored turns.
 *
 * The objective is the user's own standing instruction, and TODOs are only the
 * remaining work the WebChat itself declared. Nothing is invented, nothing is
 * stored, and every derived TODO starts `pending`, so a derived claim can never
 * arrive already complete.
 */
export function deriveGoalLedger({
  continuityId = null,
  workChainId = null,
  checkpoint = null,
  authoredTurns = [],
  declaredRemaining = [],
  todoHints = [],
  humanConstraints = [],
  objective = "",
  now = Date.now(),
} = {}) {
  const fromCheckpoint = goalLedgerFromCheckpoint(checkpoint);
  if (fromCheckpoint) {
    return normalizeGoalLedger({
      ...fromCheckpoint,
      continuity_id: continuityId || fromCheckpoint.continuity_id,
      work_chain_id: workChainId || fromCheckpoint.work_chain_id,
      updated_at: now,
    });
  }

  const turns = list(authoredTurns, SUPERVISOR_LIMITS.authoredTurns * 2)
    .map((turn) => ({
      role: String(turn?.role || "").trim().toLowerCase(),
      text: raw(turn?.text, SUPERVISOR_LIMITS.turnChars),
    }))
    .filter((turn) => (turn.role === "user" || turn.role === "assistant") && turn.text);

  const lastUser = [...turns].reverse().find((turn) => turn.role === "user");
  const hints = list(todoHints, SUPERVISOR_LIMITS.todos)
    .map((hint, index) => (typeof hint === "string"
      ? { id: `d${index + 1}`, title: hint, kind: "unknown", status: "pending", owner: "webchat" }
      : { id: txt(hint?.id, 64) || `d${index + 1}`, ...hint, status: "pending" }))
    .filter((hint) => txt(hint.title, 160));

  // Only the WebChat's own declared remaining work becomes a TODO, and only as
  // `pending`: a declaration of "not finished yet" is the one signal that is
  // safe to derive from prose.
  const declared = list(declaredRemaining, SUPERVISOR_LIMITS.todos - hints.length)
    .map((item, index) => ({
      id: txt(item?.id, 64) || `w${index + 1}`,
      title: txt(item?.title || item?.text || "", SUPERVISOR_LIMITS.todoTitle),
      kind: txt(item?.kind, 24) || "unknown",
      status: "pending",
      owner: "webchat",
    }))
    .filter((item) => item.title);

  return normalizeGoalLedger({
    source: "continuity_turns",
    continuity_id: continuityId,
    work_chain_id: workChainId,
    objective: objective || lastUser?.text || "",
    human_constraints: humanConstraints,
    todos: [...hints, ...declared],
    retry: emptyRetryBudget(),
    updated_at: now,
  });
}

export function openTodos(ledger) {
  return (ledger?.todos || []).filter((t) => t.status !== "done" && t.status !== "superseded");
}

export function isObservableTodo(todo) {
  return txt(todo?.kind, 24) !== HUMAN_OWNED_TODO_KIND;
}

/**
 * An AI self-claim of completion is never evidence. A TODO closes only with a
 * local-runtime evidence reference; `browser_result` is not proof that code ran,
 * a test passed, an agent finished, or a deploy landed.
 */
export function evidenceSatisfiesTodo(todo, evidence = null) {
  const rows = (Array.isArray(evidence) ? evidence : (todo?.evidence || []))
    .filter((e) => e && typeof e === "object" && e.kind && txt(e.ref, 200));
  if (rows.length === 0) return { ok: false, reason: "evidence_missing" };
  const herdr = rows.filter((e) => HERDR_EVIDENCE_KINDS.includes(String(e.kind)));
  if (isObservableTodo(todo)) {
    if (herdr.length === 0) return { ok: false, reason: "herdr_evidence_required" };
    return { ok: true, reason: "herdr_evidence", evidence: herdr[0] };
  }
  const human = rows.find((e) => String(e.kind) === "human");
  if (human) return { ok: true, reason: "human_evidence", evidence: human };
  if (herdr.length > 0) return { ok: true, reason: "herdr_evidence", evidence: herdr[0] };
  return { ok: false, reason: "human_evidence_required" };
}

/**
 * Apply model-proposed TODO updates under the evidence gate. A rejected update
 * never mutates the ledger, so the supervisor cannot talk itself into `done`.
 */
export function applyTodoUpdates(ledger, updates, now = Date.now()) {
  const base = normalizeGoalLedger(ledger);
  const rejections = [];
  const applied = [];
  const todos = base.todos.map((t) => ({ ...t, evidence: [...t.evidence] }));

  for (const rawUpdate of list(updates, SUPERVISOR_LIMITS.todos)) {
    const update = rawUpdate && typeof rawUpdate === "object" ? rawUpdate : null;
    const id = txt(update?.id, 64);
    if (!id) { rejections.push({ id: null, reason: "todo_id_missing" }); continue; }
    const index = todos.findIndex((t) => t.id === id);
    if (index < 0) { rejections.push({ id, reason: "unknown_todo" }); continue; }
    const nextStatus = update.status === undefined || update.status === null
      ? todos[index].status
      : oneOf(update.status, TODO_STATUSES, null);
    if (nextStatus === null) { rejections.push({ id, reason: "unknown_status" }); continue; }

    const incoming = list(update.evidence, SUPERVISOR_LIMITS.evidencePerTodo).map((item) => {
      const e = item && typeof item === "object" ? item : {};
      return {
        kind: oneOf(e.kind, EVIDENCE_KINDS, null),
        ref: txt(e.ref, SUPERVISOR_LIMITS.evidenceRef),
        summary: txt(e.summary, SUPERVISOR_LIMITS.evidenceSummary),
        at: posInt(e.at, 0) || now,
      };
    }).filter((e) => Boolean(e.kind && e.ref));

    if (nextStatus === "done") {
      const merged = [...todos[index].evidence, ...incoming].slice(-SUPERVISOR_LIMITS.evidencePerTodo);
      const check = evidenceSatisfiesTodo({ ...todos[index], status: nextStatus }, merged);
      if (!check.ok) { rejections.push({ id, reason: check.reason }); continue; }
      todos[index] = {
        ...todos[index], status: "done", evidence: merged,
        owner: txt(update.owner, 40) || todos[index].owner, updated_at: now,
      };
      applied.push({ id, status: "done", reason: check.reason });
      continue;
    }

    if (nextStatus === "superseded" && !txt(update.superseded_reason, 120)) {
      rejections.push({ id, reason: "superseded_reason_required" });
      continue;
    }
    todos[index] = {
      ...todos[index],
      status: nextStatus,
      title: txt(update.title, SUPERVISOR_LIMITS.todoTitle) || todos[index].title,
      owner: txt(update.owner, 40) || todos[index].owner,
      evidence: [...todos[index].evidence, ...incoming].slice(-SUPERVISOR_LIMITS.evidencePerTodo),
      superseded_reason: nextStatus === "superseded" ? txt(update.superseded_reason, 120) : todos[index].superseded_reason,
      updated_at: now,
    };
    applied.push({ id, status: nextStatus, reason: "status_update" });
  }

  return { ledger: normalizeGoalLedger({ ...base, todos, updated_at: now }), applied, rejections };
}

/** Completion is a property of the ledger, not of the model's prose. */
export function goalLedgerComplete(ledger) {
  const l = normalizeGoalLedger(ledger);
  if (l.todos.length === 0) return { complete: false, reason: "no_todos" };
  const open = openTodos(l);
  if (open.length > 0) return { complete: false, reason: "open_todos" };
  if (l.current_blocker) return { complete: false, reason: "blocker_open" };
  for (const todo of l.todos) {
    if (todo.status === "superseded") continue;
    const check = evidenceSatisfiesTodo(todo);
    if (!check.ok) return { complete: false, reason: check.reason, todo_id: todo.id };
  }
  return { complete: true, reason: "all_todos_closed_with_evidence" };
}

export function goalLedgerSummary(ledger) {
  const l = normalizeGoalLedger(ledger);
  const open = openTodos(l);
  return {
    derived: true,
    source: l.source,
    continuity_id: l.continuity_id,
    objective: l.objective,
    open_todos: open.length,
    open_todo_ids: open.map((t) => t.id),
    waiting_external: l.supervisor.external_owner || null,
    last_decision: l.last_decision?.decision || null,
    closed: l.closed === true,
    continue_budget: `${l.retry.continue.used}/${l.retry.continue.max}`,
    recover_budget: `${l.retry.recover.used}/${l.retry.recover.max}`,
  };
}

// ---------------------------------------------------------------------------
// 2. meaningful boundaries
// ---------------------------------------------------------------------------

const SETTLED_AGENT_STATUSES = Object.freeze(["idle", "done", "blocked", "exited", "gone"]);

/**
 * Is this observation worth one supervisor decision? Explicitly NOT: token-level
 * generation, agent_output, or repeated identical settled statuses.
 */
export function classifySupervisorBoundary(event = {}) {
  const type = String(event.type || "").trim();
  const status = String(event.status || "").trim();
  const previous = String(event.previous_status || "").trim();
  switch (type) {
    case "turn_settled":
      return { meaningful: true, boundary: "turn_settled", reason: "settled_turn" };
    case "agent_settled":
    case "agent_meaningful_change":
      if (!SETTLED_AGENT_STATUSES.includes(status)) {
        return { meaningful: false, boundary: "agent_meaningful_change", reason: "agent_not_settled" };
      }
      if (previous === status) {
        return { meaningful: false, boundary: "agent_meaningful_change", reason: "repeat_status" };
      }
      return {
        meaningful: true,
        boundary: "agent_meaningful_change",
        reason: status === "blocked" ? "agent_blocked" : "agent_completed",
      };
    case "agent_working":
    case "agent_output":
    case "generating":
      return { meaningful: false, boundary: "agent_meaningful_change", reason: "not_a_settled_transition" };
    case "silence_timeout":
      return { meaningful: true, boundary: "silence_timeout", reason: "no_progress" };
    case "provider_error":
      return { meaningful: true, boundary: "provider_error", reason: txt(event.reason, 60) || "provider_error" };
    case "browser_loss":
      return { meaningful: true, boundary: "browser_loss", reason: txt(event.reason, 60) || "browser_unavailable" };
    case "context_threshold":
      return { meaningful: true, boundary: "context_threshold", reason: txt(event.reason, 60) || "context_pressure" };
    case "handoff_settled":
      return { meaningful: true, boundary: "handoff_settled", reason: txt(event.reason, 60) || "handoff_done" };
    default:
      return { meaningful: false, boundary: null, reason: "unknown_event" };
  }
}

export function shouldRunSupervisor(ledger, boundary, now = Date.now(), policy = DEFAULT_SUPERVISOR_POLICY) {
  const l = normalizeGoalLedger(ledger);
  if (l.closed) return { run: false, reason: "goal_closed" };
  if (!boundary?.meaningful) return { run: false, reason: boundary?.reason || "not_meaningful" };
  if (!l.objective) return { run: false, reason: "no_objective" };
  const lastAt = posInt(l.supervisor.last_run_at, 0);
  const interval = posInt(policy.minBoundaryIntervalMs, 15000);
  const urgent = boundary.boundary === "provider_error" || boundary.boundary === "browser_loss";
  if (!urgent && lastAt && now - lastAt < interval) return { run: false, reason: "boundary_throttled" };
  return { run: true, reason: boundary.reason };
}

/**
 * A settled boundary may produce at most ONE action, whatever its label. The
 * legacy judge/fallback must not also nudge the same turn when the supervisor
 * already decided it. Mark the boundary owned with its assistant fingerprint;
 * a second pass for the same fingerprint is refused so no double-send happens.
 */
export function claimSupervisorBoundary(claims, convKey, fingerprint) {
  const key = `${convKey}::${fingerprint}`;
  if (claims?.has(key)) {
    return { claimed: false, reason: "boundary_already_handled" };
  }
  claims?.set(key, Date.now());
  return { claimed: true };
}

// ---------------------------------------------------------------------------
// 3. strictly bounded supervisor input
// ---------------------------------------------------------------------------

/**
 * Words that make a choice a human boundary. Deliberately over-broad: a false
 * positive only costs a human question; a false negative can publish, pay or delete.
 */
export const HUMAN_BOUNDARY_CODES = Object.freeze({
  irreversible_delete: /删除|移除|清除|销毁|清空|drop\s+table|delete|purge|destroy|rm\s+-rf|卸载|uninstall/i,
  irreversible_write: /覆盖|覆写|强制推送|force\s*push|--force|reset\s+--hard|回滚|rollback|重写历史|rewrite\s+history/i,
  payment: /支付|付费|付款|购买|订阅|充值|计费|账单|发票|billing|payment|purchase|subscribe|charge|invoice|报价|价格/i,
  publish_release: /发布|上线|公开|推到生产|deploy\s+to\s+prod|release|publish|go\s+live|生产环境|production/i,
  external_communication: /发邮件|发送邮件|回复客户|通知客户|群发|对外公告|email\s+the|notify\s+customer|press\s+release/i,
  identity_credential: /账号|账户|吊销|撤销|授权范围|密钥|证书|credential|revoke|token\s+scope|权限提升/i,
  legal_compliance: /合同|法务|合规|法律|知识产权|license\s+change|条款/i,
  subjective_preference: /审美|风格|品牌|命名|口味|个人偏好|主观|你更喜欢|更想要哪个|preference|taste|branding|aesthetics/i,
  hiring_people: /招聘|录用|解雇|绩效|工资|薪资|hire|fire|salary|compensation/i,
});

export function detectHumanBoundary(parts = []) {
  const haystack = parts.map((p) => String(p ?? "")).join("\n");
  if (!haystack.trim()) return { human_boundary: false, codes: [] };
  const codes = Object.entries(HUMAN_BOUNDARY_CODES)
    .filter(([, pattern]) => pattern.test(haystack))
    .map(([code]) => code);
  return { human_boundary: codes.length > 0, codes };
}

function sanitizeAuthoredTurn(turn, limits) {
  const role = String(turn?.role || "").trim().toLowerCase();
  if (role !== "user" && role !== "assistant") return { dropped: "non_authored_role" };
  const full = raw(turn?.text, 0).trim();
  if (!full) return { dropped: "empty" };
  const body = full.slice(0, limits.turnChars);
  // Never forward a tool body or an injected wake template as authored intent.
  if (/\{\s*"tool"|herdr_call\s*\(|^herdr\s+workspace\b/i.test(body)) return { dropped: "tool_body" };
  return { role, text: full.length > body.length ? `${body}…` : body };
}

/**
 * The only input a supervisor model may see: bounded objective, constraints,
 * open/closed TODOs, evidence summaries, a few recent authored turns, live
 * runtime state. Never the transcript, never tool bodies.
 */
export function buildSupervisorInput({
  ledger,
  authoredTurns = [],
  evidence = [],
  runtime = {},
  semanticPrior = null,
  boundary = null,
  now = Date.now(),
} = {}) {
  const L = SUPERVISOR_LIMITS;
  const l = normalizeGoalLedger(ledger);
  const dropped = { turns: 0, truncated: false };

  const turns = [];
  for (const turn of list(authoredTurns, L.authoredTurns * 3)) {
    if (turns.length >= L.authoredTurns) { dropped.turns += 1; continue; }
    const clean = sanitizeAuthoredTurn(turn, L);
    if (clean.dropped) { dropped.turns += 1; continue; }
    turns.push(clean);
  }

  const openLines = openTodos(l).map((t) => {
    const ev = t.evidence.length ? `; evidence=${t.evidence.map((e) => `${e.kind}:${e.ref}`).join(",")}` : "";
    return `- [${t.id}] ${t.title} (status=${t.status}, owner=${t.owner}, kind=${t.kind}${ev})`;
  });
  const closedLines = l.todos.filter((t) => t.status === "done").slice(0, 4).map((t) => {
    const ev = t.evidence.length ? `evidence=${t.evidence.map((e) => `${e.kind}:${e.ref}`).join(",")}` : "evidence=none";
    return `- [${t.id}] ${t.title} (${ev})`;
  });
  const evidenceLines = list(evidence, L.evidenceSummaries).map((e) => {
    const row = e && typeof e === "object" ? e : {};
    if (!row.kind || !row.ref) return null;
    const summary = txt(row.summary, L.evidenceSummary);
    return `- ${row.kind}:${txt(row.ref, L.evidenceRef)}${summary ? ` — ${summary}` : ""}`;
  }).filter(Boolean);

  const runtimeLines = [
    `agent_running=${runtime.agent_running === true}`,
    `agent_status=${txt(runtime.agent_status, 20) || "unknown"}`,
    `external_owner=${txt(runtime.external_owner, 60) || "none"}`,
    `generation_settled=${runtime.generation_settled !== false}`,
    `delivery_uncertain=${runtime.delivery_uncertain === true}`,
    `mutation_pending=${runtime.mutation_pending === true}`,
    `handoff_active=${runtime.handoff_active === true}`,
    `context_state=${txt(runtime.context_state, 32) || "healthy"}`,
    `provider_error=${txt(runtime.provider_error, 40) || "none"}`,
    `browser_online=${runtime.browser_online !== false}`,
    `continue_used=${l.retry.continue.used}/${l.retry.continue.max}`,
    `recover_used=${l.retry.recover.used}/${l.retry.recover.max}`,
    `waiting_external=${l.supervisor.external_owner || "none"}`,
  ];
  const semanticPriorLines = semanticPrior?.ok && semanticPrior?.probabilities
    ? [
      "source=jev",
      "advisory_only=true",
      ...Object.entries(semanticPrior.probabilities)
        .map(([key, value]) => `${key}=${Number(value).toFixed(3)}`),
    ]
    : ["source=none"];

  const kept = [
    ["objective", l.objective || "(none recorded)"],
    ["human_constraints", l.human_constraints.length
      ? l.human_constraints.map((c, i) => `- constraint:${i} ${c}`).join("\n") : "(none recorded)"],
    ["accepted_decisions", l.accepted_decisions.length
      ? l.accepted_decisions.map((d) => `- decision:${d.id} ${d.decision}`).join("\n") : "(none recorded)"],
    ["open_todos", openLines.length ? openLines.join("\n") : "(none)"],
    ["closed_todos", closedLines.length ? closedLines.join("\n") : "(none)"],
    ["current_blocker", l.current_blocker ? `${l.current_blocker.kind}: ${l.current_blocker.detail}` : "(none)"],
    ["evidence_summaries", evidenceLines.length ? evidenceLines.join("\n") : "(none)"],
    ["recent_authored_turns", turns.length ? turns.map((t) => `[${t.role}] ${t.text}`).join("\n") : "(none)"],
    ["live_runtime_state", runtimeLines.join("\n")],
    ["semantic_prior", semanticPriorLines.join("\n")],
    ["boundary", boundary ? `${boundary.boundary}: ${boundary.reason}` : "(none)"],
  ];
  const assemble = () => kept.map(([name, body]) => `## ${name}\n${body}`).join("\n\n");
  for (const name of ["recent_authored_turns", "evidence_summaries", "closed_todos"]) {
    if (assemble().length <= L.totalInputChars) break;
    const index = kept.findIndex(([n]) => n === name);
    if (index >= 0) { kept[index] = [kept[index][0], "(omitted: input budget)"]; dropped.truncated = true; }
  }
  let out = assemble();
  if (out.length > L.totalInputChars) {
    out = `${out.slice(0, L.totalInputChars - 1)}…`;
    dropped.truncated = true;
  }

  return {
    version: SUPERVISOR_VERSION,
    boundary: boundary ? { boundary: boundary.boundary, reason: boundary.reason } : null,
    objective: l.objective,
    open_todo_ids: openTodos(l).map((t) => t.id),
    sections: kept.map(([name, body]) => ({ name, body })),
    text: out,
    chars: out.length,
    dropped,
    fingerprint: supervisorHash([out, l.objective, l.todos.map((t) => `${t.id}:${t.status}`).join(",")].join("\u0001")).slice(0, 16),
  };
}

// ---------------------------------------------------------------------------
// 4. decision contract (strict) + risk classification
// ---------------------------------------------------------------------------

export const SUPERVISOR_SYSTEM_PROMPT = [
  "You are a deterministic goal supervisor for one WebChat conversation working on one project.",
  "You do not do the work. You decide exactly one next action.",
  "",
  "Answer with ONE JSON object and nothing else. Allowed keys only:",
  "  decision: one of COMPLETE | WAIT_EXTERNAL | CONTINUE | ANSWER_DECISION | RECOVER | HANDOFF | ASK_HUMAN",
  "  reason: short justification, <=300 chars",
  "  retry_class: none | transient | provider | browser | uncertain",
  "  todo_updates: array of {id, status, owner?, evidence?: [{kind, ref, summary?}], superseded_reason?}",
  "  message_to_send: the exact text to send to the WebChat conversation, or null",
  "  choice: {question, options: [..], chosen} — required for ANSWER_DECISION",
  "  derived_from: ledger references that justify an ANSWER_DECISION",
  "  external_owner: who is being waited on — required for WAIT_EXTERNAL",
  "",
  "Rules you cannot override:",
  "- semantic_prior is advisory only. It may help classify the situation, but ledger evidence and deterministic runtime guards remain authoritative.",
  "- A TODO may only become done with evidence whose kind starts with herdr_ and whose ref was actually observed locally. Your own prose is never evidence.",
  "- WAIT_EXTERNAL is correct whenever the WebChat is waiting on a local agent, and then you must NOT send a continue message.",
  "- CONTINUE only when the objective has genuinely unfinished work and nothing is blocking, uncertain or already in flight.",
  "- ANSWER_DECISION only for a reversible, technical choice the existing objective/constraints already determine; list derived_from. Any subjective, irreversible, paid, published, deleted or identity-affecting choice is ASK_HUMAN.",
  "- RECOVER only for a bounded, attributable failure. If delivery is uncertain, ask for reconciliation first and never replay a mutation.",
  "- HANDOFF only when conversation context pressure requires a continuity transfer.",
  "- COMPLETE only when every TODO is closed with evidence and no blocker remains.",
  "- When unsure between CONTINUE and ASK_HUMAN, choose ASK_HUMAN.",
].join("\n");

export const SUPERVISOR_DECISION_KEYS = Object.freeze([
  "decision", "reason", "retry_class", "todo_updates", "message_to_send",
  "choice", "derived_from", "external_owner",
]);

function extractJsonObject(value, maxChars = 8000) {
  const body = String(value ?? "");
  if (!body.trim()) return null;
  const fenced = body.match(/```(?:json)?\s*([\s\S]{0,8000}?)```/i);
  const source = fenced ? fenced[1] : body;
  const start = source.indexOf("{");
  if (start < 0) return null;
  let depth = 0;
  let inString = false;
  let escaped = false;
  for (let i = start; i < source.length && i - start <= maxChars; i += 1) {
    const ch = source[i];
    if (inString) {
      if (escaped) escaped = false;
      else if (ch === "\\") escaped = true;
      else if (ch === "\"") inString = false;
      continue;
    }
    if (ch === "\"") { inString = true; continue; }
    if (ch === "{") depth += 1;
    else if (ch === "}") {
      depth -= 1;
      if (depth === 0) return source.slice(start, i + 1);
    }
  }
  return null;
}

/**
 * Strict parse. Anything outside the contract — unknown decision, unknown key,
 * oversized or missing field — yields `{ok:false}`, which the caller must treat
 * as "no executable decision", never as a default action.
 */
export function parseSupervisorDecision(value) {
  const json = extractJsonObject(value);
  if (!json) return { ok: false, error: "no_json_object" };
  let parsed;
  try {
    parsed = JSON.parse(json);
  } catch {
    return { ok: false, error: "invalid_json" };
  }
  if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) return { ok: false, error: "not_an_object" };
  for (const key of Object.keys(parsed)) {
    if (!SUPERVISOR_DECISION_KEYS.includes(key)) return { ok: false, error: `unknown_field:${key}` };
  }

  const decision = oneOf(parsed.decision, SUPERVISOR_DECISIONS, null);
  if (!decision) return { ok: false, error: "unknown_decision" };

  const reason = txt(parsed.reason, SUPERVISOR_LIMITS.reason + 1);
  if (!reason) return { ok: false, error: "reason_required" };
  if (raw(parsed.reason, 0).length > SUPERVISOR_LIMITS.reason) return { ok: false, error: "reason_too_long" };

  const retryClass = parsed.retry_class === undefined || parsed.retry_class === null
    ? "none"
    : oneOf(parsed.retry_class, RETRY_CLASSES, null);
  if (retryClass === null) return { ok: false, error: "unknown_retry_class" };

  if (parsed.todo_updates !== undefined && parsed.todo_updates !== null && !Array.isArray(parsed.todo_updates)) {
    return { ok: false, error: "todo_updates_not_array" };
  }
  if (Array.isArray(parsed.todo_updates) && parsed.todo_updates.length > SUPERVISOR_LIMITS.todos) {
    return { ok: false, error: "too_many_todo_updates" };
  }
  const todoUpdates = (parsed.todo_updates || []).map((update) => {
    const u = update && typeof update === "object" ? update : {};
    return {
      id: txt(u.id, 64),
      status: u.status === undefined || u.status === null ? null : oneOf(u.status, TODO_STATUSES, null),
      owner: txt(u.owner, 40) || null,
      title: txt(u.title, SUPERVISOR_LIMITS.todoTitle) || null,
      superseded_reason: txt(u.superseded_reason, 120) || null,
      evidence: list(u.evidence, SUPERVISOR_LIMITS.evidencePerTodo).map((item) => {
        const e = item && typeof item === "object" ? item : {};
        return {
          kind: oneOf(e.kind, EVIDENCE_KINDS, null),
          ref: txt(e.ref, SUPERVISOR_LIMITS.evidenceRef),
          summary: txt(e.summary, SUPERVISOR_LIMITS.evidenceSummary),
        };
      }).filter((e) => Boolean(e.kind && e.ref)),
    };
  });
  for (const u of todoUpdates) {
    if (!u.id) return { ok: false, error: "todo_update_id_required" };
    if (u.status === null) return { ok: false, error: "todo_update_status_required" };
  }

  let message = null;
  if (parsed.message_to_send !== undefined && parsed.message_to_send !== null) {
    if (typeof parsed.message_to_send !== "string") return { ok: false, error: "message_not_string" };
    if (raw(parsed.message_to_send, 0).length > SUPERVISOR_LIMITS.message) return { ok: false, error: "message_too_long" };
    message = raw(parsed.message_to_send, SUPERVISOR_LIMITS.message);
  }

  let choice = null;
  if (parsed.choice !== undefined && parsed.choice !== null) {
    if (typeof parsed.choice !== "object" || Array.isArray(parsed.choice)) return { ok: false, error: "choice_not_object" };
    const question = txt(parsed.choice.question, 400);
    const chosen = txt(parsed.choice.chosen, 200);
    const options = list(parsed.choice.options, 8).map((o) => txt(o, 200)).filter(Boolean);
    if (!question || !chosen) return { ok: false, error: "choice_incomplete" };
    choice = { question, options, chosen };
  }

  let derivedFrom = null;
  if (parsed.derived_from !== undefined && parsed.derived_from !== null) {
    if (!Array.isArray(parsed.derived_from)) return { ok: false, error: "derived_from_not_array" };
    derivedFrom = list(parsed.derived_from, 8).map((v) => txt(v, 64)).filter(Boolean);
  }

  const externalOwner = txt(parsed.external_owner, 60) || null;

  if (decision === "WAIT_EXTERNAL" && !externalOwner) return { ok: false, error: "external_owner_required" };
  if (decision === "ANSWER_DECISION" && !choice) return { ok: false, error: "choice_required" };
  if (["CONTINUE", "ANSWER_DECISION"].includes(decision) && !message) return { ok: false, error: "message_required" };

  return {
    ok: true,
    decision,
    reason,
    retry_class: retryClass,
    todo_updates: todoUpdates,
    message_to_send: message,
    choice,
    derived_from: derivedFrom,
    external_owner: externalOwner,
  };
}

/**
 * Deterministic risk classification for a WebChat A/B/C question. The model
 * proposes; this decides whether a human must answer.
 */
export function classifyChoiceRisk({ question = "", options = [], chosen = "" } = {}) {
  const detected = detectHumanBoundary([question, chosen, ...list(options, 8)]);
  return {
    human_boundary: detected.human_boundary,
    codes: detected.codes,
    reversible: !detected.codes.includes("irreversible_delete") && !detected.codes.includes("irreversible_write"),
    technical: !detected.codes.includes("subjective_preference")
      && !detected.codes.includes("hiring_people")
      && !detected.codes.includes("legal_compliance"),
  };
}

// ---------------------------------------------------------------------------
// 5. deterministic guard
// ---------------------------------------------------------------------------

function runtimeOf(runtime) {
  const r = runtime && typeof runtime === "object" ? runtime : {};
  const agentStatus = txt(r.agent_status, 20) || "unknown";
  return {
    agent_running: r.agent_running === true || agentStatus === "working",
    agent_status: agentStatus,
    external_owner: txt(r.external_owner, 60) || null,
    generation_settled: r.generation_settled !== false,
    delivery_uncertain: r.delivery_uncertain === true,
    mutation_pending: r.mutation_pending === true,
    handoff_active: r.handoff_active === true,
    handoff_capable: r.handoff_capable !== false,
    bound: r.bound !== false,
    protocol_conversation: r.protocol_conversation !== false,
    context_state: txt(r.context_state, 32) || "healthy",
    provider_error: txt(r.provider_error, 40) || null,
    browser_online: r.browser_online !== false,
    // The exact set of locally observed evidence references.
    observed_evidence_refs: list(r.observed_evidence_refs, 40).map((ref) => txt(ref, SUPERVISOR_LIMITS.evidenceRef)).filter(Boolean),
  };
}

/** A model may cite evidence; it may not invent it. */
function verifyTodoEvidenceRefs(decision, r) {
  const unknown = [];
  for (const update of decision.todo_updates || []) {
    if (update.status !== "done") continue;
    if (!update.evidence || update.evidence.length === 0) { unknown.push(`${update.id}:no_evidence`); continue; }
    for (const item of update.evidence) {
      if (!r.observed_evidence_refs.includes(item.ref)) unknown.push(`${update.id}:${item.ref}`);
    }
  }
  return { ok: unknown.length === 0, unknown };
}

function effectsFor(decision, parsed, ledger) {
  return {
    decision,
    send: parsed?.message_to_send ? { text: parsed.message_to_send, kind: decision } : null,
    todo_updates: parsed?.todo_updates || [],
    retry_class: parsed?.retry_class || "none",
    wait_external: decision === "WAIT_EXTERNAL" ? { owner: parsed?.external_owner || null } : null,
    ask_human: decision === "ASK_HUMAN" ? { question: parsed?.choice || null, reason: parsed?.reason || null } : null,
    handoff: decision === "HANDOFF" ? { reason: parsed?.reason || null, boundary: "context_threshold" } : null,
    reconcile: decision === "RECOVER" ? { required: null, replay_forbidden: true } : null,
    complete: decision === "COMPLETE",
    stop_nudging: decision === "COMPLETE" || decision === "WAIT_EXTERNAL" || decision === "ASK_HUMAN",
    retry_budget: {
      continue_used: ledger.retry.continue.used,
      continue_max: ledger.retry.continue.max,
      recover_used: ledger.retry.recover.used,
      recover_max: ledger.retry.recover.max,
    },
  };
}

/** A denied decision never carries an executable send. `safeDecision` is the safe state to report. */
function denied(reason, safeDecision, detail = null) {
  return { allowed: false, reason, safeDecision, detail, effects: null };
}

/** Only a ledger-progress mismatch is worth one bounded re-ask. */
const REASKABLE_DENIALS = Object.freeze([
  "open_todos", "no_todos", "blocker_open", "herdr_evidence_required",
  "unverified_evidence_ref", "goal_closed", "nothing_left_to_continue",
]);

/** Resolve `derived_from` against the ledger: only existing references count. */
export function resolveLedgerReferences(ledger, refs = []) {
  const l = normalizeGoalLedger(ledger);
  const resolved = [];
  const unknown = [];
  for (const ref of list(refs, 8)) {
    const value = txt(ref, 64);
    if (!value) continue;
    if (value === "objective" || value === "constraint" || value === "todos") { resolved.push(value); continue; }
    if (value.startsWith("constraint:")) {
      const index = Number(value.slice("constraint:".length));
      if (Number.isInteger(index) && index >= 0 && index < l.human_constraints.length) resolved.push(value);
      else unknown.push(value);
      continue;
    }
    if (value.startsWith("decision:")) {
      if (l.accepted_decisions.some((d) => d.id === value.slice("decision:".length))) resolved.push(value);
      else unknown.push(value);
      continue;
    }
    if (value.startsWith("todo:")) {
      if (l.todos.some((t) => t.id === value.slice("todo:".length))) resolved.push(value);
      else unknown.push(value);
      continue;
    }
    unknown.push(value);
  }
  return { resolved, unknown };
}

/** The only place that decides whether a proposed decision may execute. */
export function guardSupervisorDecision(decision, { runtime = {}, ledger = null, policy = DEFAULT_SUPERVISOR_POLICY } = {}) {
  if (!decision?.ok) return denied("invalid_decision", "ASK_HUMAN", decision?.error || null);
  const l = normalizeGoalLedger(ledger);
  const r = runtimeOf(runtime);
  const kind = decision.decision;
  const effects = effectsFor(kind, decision, l);

  // Width guard: a message carrying a human-boundary action is never sent
  // autonomously, whatever the decision label says.
  if (effects.send && kind !== "ASK_HUMAN") {
    const boundary = detectHumanBoundary([effects.send.text]);
    if (boundary.human_boundary) return denied("human_decision_boundary", "ASK_HUMAN", boundary.codes);
  }
  // Evidence-integrity guard: a cited reference must have been observed locally.
  const evidenceCheck = verifyTodoEvidenceRefs(decision, r);
  if (!evidenceCheck.ok) return denied("unverified_evidence_ref", "CONTINUE", { refs: evidenceCheck.unknown });

  switch (kind) {
    case "ASK_HUMAN":
      return { allowed: true, decision, reason: decision.reason, effects };

    case "COMPLETE": {
      if (r.delivery_uncertain || r.mutation_pending) return denied("mutation_uncertain_reconcile_required", "RECOVER");
      if (!r.generation_settled) return denied("generation_not_settled", "WAIT_EXTERNAL");
      const completion = goalLedgerComplete(l);
      if (!completion.complete) return denied(completion.reason, "CONTINUE", { todo_id: completion.todo_id || null });
      return { allowed: true, decision, reason: decision.reason, effects };
    }

    case "WAIT_EXTERNAL": {
      // Only locally observed external work makes waiting real.
      const owner = r.external_owner || (r.agent_running ? "herdr_agent" : null);
      if (!owner) return denied("no_external_owner_running", "CONTINUE");
      return { allowed: true, decision, reason: decision.reason, effects: { ...effects, send: null, wait_external: { owner } } };
    }

    case "CONTINUE": {
      if (r.delivery_uncertain || r.mutation_pending) return denied("mutation_uncertain_reconcile_required", "RECOVER");
      if (!r.generation_settled) return denied("generation_not_settled", "WAIT_EXTERNAL");
      if (r.handoff_active) return denied("handoff_in_flight", "WAIT_EXTERNAL");
      if (r.agent_running || r.external_owner) return denied("external_owner_running", "WAIT_EXTERNAL", { owner: r.external_owner });
      if (l.closed) return denied("goal_closed", "COMPLETE");
      if (openTodos(l).length === 0 && !l.current_blocker) return denied("nothing_left_to_continue", "COMPLETE");
      if (l.retry.continue.used >= l.retry.continue.max) {
        return denied("retry_budget_exhausted", "ASK_HUMAN", { used: l.retry.continue.used, max: l.retry.continue.max });
      }
      return { allowed: true, decision, reason: decision.reason, effects };
    }

    case "ANSWER_DECISION": {
      if (r.delivery_uncertain || r.mutation_pending) return denied("mutation_uncertain_reconcile_required", "RECOVER");
      if (!r.generation_settled) return denied("generation_not_settled", "WAIT_EXTERNAL");
      const risk = classifyChoiceRisk(decision.choice || {});
      if (risk.human_boundary) return denied("human_decision_boundary", "ASK_HUMAN", risk.codes);
      if (!risk.technical || !risk.reversible) return denied("choice_not_reversible_technical", "ASK_HUMAN", risk.codes);
      // Derivability is proven against the ledger, not asserted by the model.
      const refs = resolveLedgerReferences(l, decision.derived_from || []);
      if (refs.unknown.length > 0 || refs.resolved.length === 0) {
        return denied("not_derivable_from_goal", "ASK_HUMAN", { unknown_refs: refs.unknown });
      }
      return { allowed: true, decision, reason: decision.reason, effects: { ...effects, derivation: { refs: refs.resolved, codes: risk.codes } } };
    }

    case "RECOVER": {
      if (l.retry.recover.used >= l.retry.recover.max) {
        return denied("retry_budget_exhausted", "ASK_HUMAN", { used: l.retry.recover.used, max: l.retry.recover.max });
      }
      const recoverable = r.delivery_uncertain || r.mutation_pending || Boolean(r.provider_error)
        || !r.browser_online || r.agent_status === "blocked";
      if (!recoverable) return denied("no_recoverable_condition", "CONTINUE");
      if (decision.retry_class === "none") return denied("recover_requires_retry_class", "RECOVER");
      // An uncertain mutation is reconciliation-only: the guard strips the send
      // so a blind replay is structurally impossible.
      if (r.delivery_uncertain || r.mutation_pending || decision.retry_class === "uncertain") {
        return { allowed: true, decision, reason: decision.reason, effects: { ...effects, send: null, reconcile: { required: true, replay_forbidden: true } } };
      }
      return { allowed: true, decision, reason: decision.reason, effects };
    }

    case "HANDOFF": {
      if (r.handoff_active) return denied("handoff_in_flight", "WAIT_EXTERNAL");
      if (r.delivery_uncertain || r.mutation_pending) return denied("handoff_unsafe_while_uncertain", "ASK_HUMAN");
      if (!r.bound || !r.handoff_capable) return denied("handoff_not_available", "ASK_HUMAN");
      if (!r.protocol_conversation) return denied("handoff_requires_project_conversation", "ASK_HUMAN");
      const threshold = policy.handoffStates || DEFAULT_SUPERVISOR_POLICY.handoffStates;
      if (!threshold.includes(r.context_state)) {
        return denied("context_below_handoff_threshold", "CONTINUE", { context_state: r.context_state });
      }
      return { allowed: true, decision, reason: decision.reason, effects: { ...effects, send: null } };
    }

    default:
      return denied("invalid_decision", "ASK_HUMAN");
  }
}

// ---------------------------------------------------------------------------
// 6. bounded provider adapter (transport injected; no provider/model/key here)
// ---------------------------------------------------------------------------

export function buildSupervisorUserMessage(input) {
  return [
    "BOUNDED SUPERVISOR INPUT (already trimmed; do not ask for more):",
    "",
    input.text,
    "",
    "Decide the single next action and return only the JSON object.",
  ].join("\n");
}

/**
 * Wrap an injected transport into the bounded supervisor adapter.
 * `request({system, user, timeoutMs})` is the ONLY provider surface, and it only
 * ever receives the projection from `buildSupervisorInput`.
 */
export function createBoundedSupervisorAdapter({
  request,
  isConfigured = () => true,
  maxAttempts = DEFAULT_SUPERVISOR_POLICY.maxDecisionAttempts,
  timeoutMs = 45000,
} = {}) {
  const attempts = posInt(maxAttempts, 2);

  async function run(input, { correction = null } = {}) {
    if (typeof request !== "function") return { ok: false, reason: "no_transport" };
    if (!isConfigured()) return { ok: false, reason: "not_configured" };
    if (!input || typeof input.text !== "string" || !input.text.trim()) return { ok: false, reason: "empty_input" };
    if (input.text.length > SUPERVISOR_LIMITS.totalInputChars) return { ok: false, reason: "input_over_budget" };

    let last = { ok: false, reason: "no_attempt" };
    let user = buildSupervisorUserMessage(input);
    if (correction) user = `${user}\n\nYour previous answer was rejected by the deterministic guard: ${correction}. Return a corrected JSON object.`;
    for (let attempt = 1; attempt <= attempts; attempt += 1) {
      const result = await request({ system: SUPERVISOR_SYSTEM_PROMPT, user, timeoutMs });
      if (!result?.ok) {
        last = { ok: false, reason: result?.reason || "provider_error" };
        if (!["timeout", "network"].includes(last.reason)) break;
        continue;
      }
      const parsed = parseSupervisorDecision(result.content);
      if (!parsed.ok) {
        last = { ok: false, reason: `invalid_decision:${parsed.error}` };
        user = `${user}\n\nYour previous answer was rejected: ${parsed.error}. Return only the required JSON object.`;
        continue;
      }
      return { ok: true, decision: parsed, attempts: attempt };
    }
    return { ok: false, reason: last.reason, attempts };
  }

  return { run, maxAttempts: attempts };
}

// ---------------------------------------------------------------------------
// 7. orchestration
// ---------------------------------------------------------------------------

/**
 * One supervisor turn. The only side effect is the injected transport call; the
 * only state returned is the next derived ledger revision, which the caller
 * keeps as bounded session bookkeeping (never as an authority).
 */
export async function supervise({
  ledger,
  boundaryEvent = null,
  boundary = null,
  authoredTurns = [],
  evidence = [],
  runtime = {},
  semanticPrior = null,
  adapter,
  now = Date.now(),
  policy = DEFAULT_SUPERVISOR_POLICY,
  carry = null,
} = {}) {
  let base = normalizeGoalLedger(ledger);
  if (carry && typeof carry === "object") {
    // Session bookkeeping only: budgets, pacing and wait state. The objective
    // and TODOs always come from the freshly derived ledger.
    base = normalizeGoalLedger({
      ...base,
      retry: carry.retry || base.retry,
      supervisor: { ...base.supervisor, ...(carry.supervisor || {}) },
    });
  }

  const classified = boundary || classifySupervisorBoundary(boundaryEvent || {});
  const gate = shouldRunSupervisor(base, classified, now, policy);
  if (!gate.run) {
    return { ok: false, status: "not_run", reason: gate.reason, boundary: classified, ledger: base, effects: null, llm_calls: 0 };
  }
  if (!adapter || typeof adapter.run !== "function") {
    return { ok: false, status: "no_adapter", reason: "adapter_missing", boundary: classified, ledger: base, effects: null, llm_calls: 0 };
  }

  const input = buildSupervisorInput({
    ledger: base,
    authoredTurns,
    evidence,
    runtime,
    semanticPrior,
    boundary: classified,
    now,
  });
  const maxAttempts = posInt(policy.maxDecisionAttempts, 2);
  let llmCalls = 0;
  let correction = null;
  let outcome = null;
  let guard = null;

  for (let attempt = 1; attempt <= maxAttempts; attempt += 1) {
    llmCalls += 1;
    outcome = await adapter.run(input, { correction });
    if (!outcome.ok) break;
    guard = guardSupervisorDecision(outcome.decision, { runtime, ledger: base, policy });
    if (guard.allowed) break;
    if (!REASKABLE_DENIALS.includes(guard.reason) || attempt >= maxAttempts) break;
    correction = `${guard.reason} (re-check the ledger: CONTINUE only if real unfinished work remains, otherwise COMPLETE or ASK_HUMAN)`;
  }

  const withRun = normalizeGoalLedger({
    ...base,
    supervisor: { ...base.supervisor, last_boundary: classified.boundary, last_run_at: now, last_input_fingerprint: input.fingerprint },
    updated_at: now,
  });

  if (!outcome?.ok) {
    return {
      ok: false, status: "provider_failed", reason: outcome?.reason || "provider_error",
      boundary: classified, effects: null, input, llm_calls: llmCalls,
      ledger: normalizeGoalLedger({
        ...withRun,
        last_decision: { decision: null, reason: `provider:${outcome?.reason || "error"}`, retry_class: "provider", at: now },
      }),
    };
  }
  if (!guard) guard = guardSupervisorDecision(outcome.decision, { runtime, ledger: base, policy });

  if (!guard.allowed) {
    return {
      ok: false, status: "guard_denied", reason: guard.reason, safeDecision: guard.safeDecision, detail: guard.detail,
      boundary: classified, effects: null, input, llm_calls: llmCalls,
      ledger: normalizeGoalLedger({
        ...withRun,
        last_decision: { decision: "ASK_HUMAN", reason: `guard:${guard.reason}`, retry_class: "none", at: now },
      }),
    };
  }

  const decision = guard.decision;
  const applied = applyTodoUpdates(withRun, guard.effects.todo_updates, now);
  const next = applied.ledger;
  const retry = { continue: { ...next.retry.continue }, recover: { ...next.retry.recover } };
  if (decision.decision === "CONTINUE") retry.continue.used += 1;
  if (decision.decision === "RECOVER") retry.recover.used += 1;

  const finalLedger = normalizeGoalLedger({
    ...next,
    retry,
    closed: guard.effects.complete === true,
    current_blocker: guard.effects.complete === true ? null : next.current_blocker,
    supervisor: {
      ...next.supervisor,
      external_owner: decision.decision === "WAIT_EXTERNAL" ? (guard.effects.wait_external?.owner || null) : null,
      waiting_since: decision.decision === "WAIT_EXTERNAL" ? (next.supervisor.waiting_since || now) : null,
    },
    last_decision: { decision: decision.decision, reason: decision.reason, retry_class: decision.retry_class, at: now },
    updated_at: now,
  });

  return {
    ok: true,
    status: "decided",
    decision: decision.decision,
    reason: decision.reason,
    retry_class: decision.retry_class,
    boundary: classified,
    ledger: finalLedger,
    effects: { ...guard.effects, todo_applied: applied.applied, todo_rejected: applied.rejections },
    input,
    llm_calls: llmCalls,
  };
}

/**
 * Deterministic bounded recovery when the supervisor transport itself fails
 * (timeout / provider / browser loss). No model output exists in that case, so
 * the runtime plans the recovery and still routes it through the guard: the
 * budget is honoured, no message is produced, and an uncertain mutation stays
 * reconciliation-only.
 */
export function planTransportRecovery({
  ledger,
  runtime = {},
  reason = "provider_error",
  boundary = "provider_error",
  now = Date.now(),
  policy = DEFAULT_SUPERVISOR_POLICY,
} = {}) {
  const base = normalizeGoalLedger(ledger);
  const short = txt(reason, 120) || "provider_error";
  const proposal = {
    ok: true,
    decision: "RECOVER",
    reason: `supervisor transport failed: ${short}`,
    retry_class: "provider",
    todo_updates: [],
    message_to_send: null,
    choice: null,
    derived_from: null,
    external_owner: null,
  };
  const guard = guardSupervisorDecision(proposal, { runtime: { ...runtime, provider_error: short }, ledger: base, policy });
  if (!guard.allowed) {
    return {
      allowed: false,
      decision: "ASK_HUMAN",
      reason: guard.reason,
      effects: null,
      ledger: normalizeGoalLedger({
        ...base,
        last_decision: { decision: "ASK_HUMAN", reason: `guard:${guard.reason}`, retry_class: "none", at: now },
        updated_at: now,
      }),
    };
  }
  return {
    allowed: true,
    decision: "RECOVER",
    reason: "bounded_recover",
    effects: { ...guard.effects, send: null },
    ledger: normalizeGoalLedger({
      ...base,
      retry: { ...base.retry, recover: { ...base.retry.recover, used: base.retry.recover.used + 1 } },
      supervisor: { ...base.supervisor, last_boundary: boundary, last_run_at: now },
      last_decision: { decision: "RECOVER", reason: proposal.reason, retry_class: "provider", at: now },
      updated_at: now,
    }),
  };
}