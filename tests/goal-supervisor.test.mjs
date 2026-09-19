import assert from "node:assert/strict";
import test from "node:test";

import {
  DEFAULT_SUPERVISOR_POLICY,
  SUPERVISOR_DECISIONS,
  SUPERVISOR_LIMITS,
  applyTodoUpdates,
  buildSupervisorInput,
  claimSupervisorBoundary,
  classifyCheckpointPutError,
  classifyChoiceRisk,
  classifySupervisorBoundary,
  createBoundedSupervisorAdapter,
  deriveGoalLedger,
  goalLedgerComplete,
  goalLedgerFromCheckpoint,
  guardSupervisorDecision,
  mergeGoalCheckpoint,
  normalizeGoalLedger,
  openTodos,
  parseSupervisorDecision,
  planTransportRecovery,
  resolveAuthoritativeWorkMemoryLocator,
  supervise,
} from "../extension/goal-supervisor-core.js";

// Real domain fakes: nothing is mocked except the provider transport itself.
function adapterFor(responses) {
  const calls = [];
  const request = async (payload) => {
    calls.push(payload);
    const next = responses.shift();
    if (typeof next === "function") return next(payload);
    if (next === undefined) return { ok: false, reason: "no_more_responses" };
    return { ok: true, content: typeof next === "string" ? next : JSON.stringify(next) };
  };
  return { calls, adapter: createBoundedSupervisorAdapter({ request }) };
}

const OBJ = "\u4fee\u590d\u672c\u5730\u6784\u5efa\u811a\u672c\u5e76\u8dd1\u901a\u6d4b\u8bd5";

function pendingTodo(extra = {}) {
  return { id: "t1", title: "\u4fee\u590d build \u811a\u672c", kind: "code", status: "pending", owner: "webchat", ...extra };
}

function ledger(todos = [pendingTodo()], extra = {}) {
  return deriveGoalLedger({
    continuityId: "hc:test:1",
    objective: OBJ,
    humanConstraints: ["\u4e0d\u5f97\u6539\u52a8\u751f\u4ea7\u914d\u7f6e"],
    todoHints: todos,
    ...extra,
    now: 1000,
  });
}

const healthyRuntime = {
  agent_running: false,
  agent_status: "idle",
  generation_settled: true,
  delivery_uncertain: false,
  mutation_pending: false,
  handoff_active: false,
  context_state: "healthy",
  bound: true,
  protocol_conversation: true,
};

// ---------------------------------------------------------------------------
// A. unfinished objective -> goal-aware CONTINUE
// ---------------------------------------------------------------------------

test("user gets a goal-aware continue | Given an unfinished objective with an open TODO and no local agent running | When the settled turn boundary reaches the supervisor | Then it decides CONTINUE and sends a message naming the objective and the open TODO", async () => {
  const { adapter, calls } = adapterFor([{
    decision: "CONTINUE",
    reason: "t1 is still pending",
    retry_class: "transient",
    todo_updates: [{ id: "t1", status: "working", owner: "webchat" }],
    message_to_send: `\u7ee7\u7eed\uff1a\u5b8c\u6210 t1\uff08${OBJ}\uff09`,
  }]);
  const result = await supervise({
    ledger: ledger(),
    boundaryEvent: { type: "turn_settled" },
    runtime: healthyRuntime,
    adapter,
    now: 10_000,
  });

  assert.equal(result.ok, true);
  assert.equal(result.decision, "CONTINUE");
  assert.equal(result.effects.send.kind, "CONTINUE");
  assert.equal(result.effects.send.text.includes(OBJ), true);
  assert.equal(result.effects.send.text.includes("t1"), true);
  assert.equal(result.ledger.todos[0].status, "working");
  assert.equal(result.ledger.retry.continue.used, 1);
  assert.equal(calls.length, 1);
});

test("user does not get nudged on an uncertain turn | Given a settled turn whose delivery state is uncertain | When the supervisor proposes CONTINUE | Then the guard refuses the send and asks for reconciliation instead", async () => {
  const { adapter } = adapterFor([{ decision: "CONTINUE", reason: "keep going", message_to_send: "\u7ee7\u7eed" }]);
  const result = await supervise({
    ledger: ledger(),
    boundaryEvent: { type: "turn_settled" },
    runtime: { ...healthyRuntime, delivery_uncertain: true },
    adapter,
    now: 10_000,
  });
  assert.equal(result.status, "guard_denied");
  assert.equal(result.reason, "mutation_uncertain_reconcile_required");
  assert.equal(result.safeDecision, "RECOVER");
  assert.equal(result.effects, null);
});

// ---------------------------------------------------------------------------
// B. waiting on the local agent -> WAIT_EXTERNAL, never a continue
// ---------------------------------------------------------------------------

test("user is not interrupted while a local agent is still working | Given the WebChat turn settled while a bound Herdr agent is working | When the supervisor runs | Then it decides WAIT_EXTERNAL with no message and no continue retry", async () => {
  const { adapter } = adapterFor([{ decision: "WAIT_EXTERNAL", reason: "\u7b49\u672c\u5730 agent \u5b8c\u6210", external_owner: "herdr_agent:pi" }]);
  const result = await supervise({
    ledger: ledger([pendingTodo({ status: "working", owner: "herdr_agent:pi" })]),
    boundaryEvent: { type: "turn_settled" },
    runtime: { ...healthyRuntime, agent_running: true, agent_status: "working", external_owner: "herdr_agent:pi" },
    adapter,
    now: 10_000,
  });

  assert.equal(result.decision, "WAIT_EXTERNAL");
  assert.equal(result.effects.send, null);
  assert.equal(result.effects.stop_nudging, true);
  assert.equal(result.effects.wait_external.owner, "herdr_agent:pi");
  assert.equal(result.ledger.supervisor.external_owner, "herdr_agent:pi");
  assert.equal(result.ledger.retry.continue.used, 0);
});

test("user is protected from a fabricated wait | Given no local agent is running | When the supervisor claims WAIT_EXTERNAL on its own | Then the guard refuses it and names CONTINUE as the safe direction", async () => {
  const { adapter } = adapterFor([{ decision: "WAIT_EXTERNAL", reason: "\u7b49\u4e00\u4e0b", external_owner: "herdr_agent:none" }]);
  const result = await supervise({
    ledger: ledger(),
    boundaryEvent: { type: "turn_settled" },
    runtime: healthyRuntime,
    adapter,
    now: 10_000,
  });
  assert.equal(result.status, "guard_denied");
  assert.equal(result.reason, "no_external_owner_running");
  assert.equal(result.safeDecision, "CONTINUE");
  assert.equal(result.effects, null);
});

test("user is woken only by a meaningful agent transition | Given a working agent that only emits output, then settles as blocked, then repeats | When each observation reaches the boundary classifier | Then only the first settled transition is a supervisor boundary", () => {
  assert.equal(classifySupervisorBoundary({ type: "agent_working", status: "working" }).meaningful, false);
  assert.equal(classifySupervisorBoundary({ type: "agent_output", status: "working" }).meaningful, false);
  assert.equal(classifySupervisorBoundary({ type: "agent_settled", status: "running" }).meaningful, false);
  const blocked = classifySupervisorBoundary({ type: "agent_settled", status: "blocked", previous_status: "working" });
  const done = classifySupervisorBoundary({ type: "agent_settled", status: "done", previous_status: "working" });
  const repeat = classifySupervisorBoundary({ type: "agent_settled", status: "done", previous_status: "done" });
  assert.equal(blocked.meaningful, true);
  assert.equal(blocked.boundary, "agent_meaningful_change");
  assert.equal(done.meaningful, true);
  assert.equal(done.reason, "agent_completed");
  assert.equal(repeat.meaningful, false);
});

test("user is woken again after a blocked agent is resolved | Given WAIT_EXTERNAL was recorded and the agent has now settled as done | When the agent transition boundary reaches the supervisor | Then it runs again and can continue the work", async () => {
  const waiting = ledger([pendingTodo({ status: "working", owner: "herdr_agent:pi" })], {
    checkpoint: { goal_supervisor: { objective: OBJ, todos: [pendingTodo({ status: "working" })], supervisor: { external_owner: "herdr_agent:pi" } } },
  });
  const { adapter } = adapterFor([{ decision: "CONTINUE", reason: "local agent finished", message_to_send: "\u672c\u5730 agent \u5df2\u5b8c\u6210\uff0c\u7ee7\u7eed\u6536\u5c3e t1" }]);
  const result = await supervise({
    ledger: waiting,
    boundaryEvent: { type: "agent_settled", status: "done", previous_status: "working" },
    runtime: { ...healthyRuntime, agent_status: "done" },
    adapter,
    now: 30_000,
  });
  assert.equal(result.decision, "CONTINUE");
  assert.equal(result.ledger.supervisor.external_owner, null);
});

// ---------------------------------------------------------------------------
// C. A/B/C choices -> ANSWER_DECISION vs ASK_HUMAN
// ---------------------------------------------------------------------------

test("user is spared a reversible technical question | Given a technical either/or question the objective and an accepted constraint already determine | When the supervisor answers it citing those ledger references | Then the answer is sent with the proven derivation", async () => {
  const base = ledger([pendingTodo()], {
    checkpoint: {
      goal_supervisor: {
        objective: OBJ,
        human_constraints: ["\u4e0d\u5f97\u6539\u52a8\u751f\u4ea7\u914d\u7f6e"],
        accepted_decisions: [{ id: "d1", decision: "\u4f18\u5148\u4fdd\u6301\u672c\u5730\u517c\u5bb9\u6027", derived_from: ["objective"] }],
        todos: [pendingTodo()],
      },
    },
  });
  const { adapter } = adapterFor([{
    decision: "ANSWER_DECISION",
    reason: "choice follows an accepted engineering decision",
    choice: { question: "\u62bd\u53d6 helper \u65f6\u7528 A \u65b9\u6848\u8fd8\u662f B \u65b9\u6848\uff1f", options: ["A", "B"], chosen: "A" },
    derived_from: ["decision:d1", "constraint:0"],
    message_to_send: "\u6309 A \u65b9\u6848\u7ee7\u7eed\uff1a\u62bd\u51fa helper\uff0c\u5e76\u5728\u672c\u5730\u8dd1\u6d4b\u8bd5\u3002",
  }]);
  const result = await supervise({ ledger: base, boundaryEvent: { type: "turn_settled" }, runtime: healthyRuntime, adapter, now: 10_000 });

  assert.equal(result.decision, "ANSWER_DECISION");
  assert.equal(result.effects.send.kind, "ANSWER_DECISION");
  assert.deepEqual(result.effects.derivation.refs, ["decision:d1", "constraint:0"]);
});

test("user answers the risky choices themselves | Given options that delete, pay, publish or judge taste | When each is classified | Then every one is a human boundary and none is answerable autonomously", async () => {
  const cases = [
    { question: "\u8981\u5220\u9664\u65e7\u7684 migration \u76ee\u5f55\u5417\uff1f", options: ["\u5220\u9664", "\u4fdd\u7559"] },
    { question: "\u8981\u8d2d\u4e70\u4f01\u4e1a\u7248\u5417\uff1f", options: ["\u8d2d\u4e70", "\u4e0d\u4e70"] },
    { question: "\u73b0\u5728\u53d1\u5e03\u5230\u751f\u4ea7\u5417\uff1f", options: ["\u53d1\u5e03", "\u7b49\u4e00\u7b49"] },
    { question: "\u54c1\u724c\u98ce\u683c\u66f4\u504f\u5411\u54ea\u4e00\u79cd\uff1f", options: ["\u7b80\u6d01", "\u6d3b\u6cfc"] },
    { question: "Should I --force push the rewritten branch?", options: ["yes", "no"] },
  ];
  for (const item of cases) assert.equal(classifyChoiceRisk(item).human_boundary, true, JSON.stringify(item));
  assert.equal(classifyChoiceRisk(cases[3]).technical, false);
  assert.equal(classifyChoiceRisk(cases[4]).reversible, false);
});

test("user is asked instead of answered for a risky choice | Given a WebChat payment question | When the supervisor proposes ANSWER_DECISION with a chosen option | Then the guard refuses and asks the human", async () => {
  const { adapter } = adapterFor([{
    decision: "ANSWER_DECISION",
    reason: "cheapest option matches constraints",
    choice: { question: "\u8981\u8d2d\u4e70\u4f01\u4e1a\u7248\u5417\uff1f", options: ["\u8d2d\u4e70", "\u4e0d\u4e70"], chosen: "\u8d2d\u4e70" },
    derived_from: ["objective"],
    message_to_send: "\u8d2d\u4e70\u4f01\u4e1a\u7248\u3002",
  }]);
  const result = await supervise({ ledger: ledger(), boundaryEvent: { type: "turn_settled" }, runtime: healthyRuntime, adapter, now: 10_000 });
  assert.equal(result.status, "guard_denied");
  assert.equal(result.safeDecision, "ASK_HUMAN");
  assert.equal(result.effects, null);
});

test("user is asked when an answer is not derivable from the goal | Given a reversible technical question with no ledger reference behind it, then an unknown reference | When the supervisor answers | Then the guard asks the human in both cases", async () => {
  const proposal = (derivedFrom) => ({
    decision: "ANSWER_DECISION",
    reason: "seems fine",
    choice: { question: "\u62bd\u53d6 helper \u65f6\u7528 A \u8fd8\u662f B\uff1f", options: ["A", "B"], chosen: "A" },
    ...(derivedFrom ? { derived_from: derivedFrom } : {}),
    message_to_send: "\u7528 A\u3002",
  });
  for (const refs of [null, ["constraint:7"]]) {
    const { adapter } = adapterFor([proposal(refs)]);
    const result = await supervise({ ledger: ledger(), boundaryEvent: { type: "turn_settled" }, runtime: healthyRuntime, adapter, now: 10_000 });
    assert.equal(result.status, "guard_denied");
    assert.equal(result.reason, "not_derivable_from_goal");
    assert.equal(result.safeDecision, "ASK_HUMAN");
  }
});

// ---------------------------------------------------------------------------
// D. completion is evidence-gated
// ---------------------------------------------------------------------------

test("user is not told the work is finished on the model's word | Given a pending code TODO and a supervisor claiming COMPLETE | When the guard checks the ledger | Then completion is refused, nothing is sent, and the TODO stays pending", async () => {
  const base = ledger();
  const { adapter, calls } = adapterFor([
    { decision: "COMPLETE", reason: "all done, trust me" },
    { decision: "CONTINUE", reason: "t1 is still pending", message_to_send: "\u7ee7\u7eed\u5b8c\u6210 t1" },
  ]);
  const result = await supervise({ ledger: base, boundaryEvent: { type: "turn_settled" }, runtime: healthyRuntime, adapter, now: 10_000 });

  assert.equal(calls.length, 2);
  assert.equal(result.decision, "CONTINUE");
  assert.equal(result.ledger.closed, false);
  assert.equal(result.ledger.todos[0].status, "pending");

  const direct = guardSupervisorDecision(
    parseSupervisorDecision('{"decision":"COMPLETE","reason":"all done"}'),
    { runtime: healthyRuntime, ledger: base },
  );
  assert.equal(direct.allowed, false);
  assert.equal(direct.reason, "open_todos");
  assert.equal(direct.safeDecision, "CONTINUE");
  assert.equal(direct.effects, null);
});

test("user observable TODO cannot be checked off by prose or invented evidence | Given a code TODO | When a done update carries only human evidence, then an unobserved Herdr reference | Then both are refused", () => {
  const prose = applyTodoUpdates(ledger(), [{ id: "t1", status: "done", evidence: [{ kind: "human", ref: "assistant said so" }] }], 12_000);
  assert.equal(prose.ledger.todos[0].status, "pending");
  assert.deepEqual(prose.rejections, [{ id: "t1", reason: "herdr_evidence_required" }]);

  const invented = guardSupervisorDecision(
    parseSupervisorDecision(JSON.stringify({
      decision: "CONTINUE",
      reason: "t1 verified",
      todo_updates: [{ id: "t1", status: "done", evidence: [{ kind: "herdr_exec", ref: "exec:made-up-42" }] }],
      message_to_send: "\u5df2\u5b8c\u6210 t1",
    })),
    { runtime: { ...healthyRuntime, observed_evidence_refs: ["exec:real-1"] }, ledger: ledger() },
  );
  assert.equal(invented.allowed, false);
  assert.equal(invented.reason, "unverified_evidence_ref");
  assert.equal(invented.effects, null);
});

test("user WebChat settlement is not proof of local work | Given a done code TODO from the Work Memory checkpoint whose only evidence is a settled browser turn | When completion is evaluated | Then the ledger is not complete", () => {
  const check = goalLedgerComplete(ledger(undefined, {
    checkpoint: {
      goal_supervisor: {
        objective: OBJ,
        todos: [pendingTodo({ status: "done", evidence: [{ kind: "browser_result", ref: "br_session_1/assistant-1" }] })],
      },
    },
  }));
  assert.equal(check.complete, false);
  assert.equal(check.reason, "herdr_evidence_required");
});

test("user is told the work is finished once every TODO carries local evidence | Given all TODOs closed with Herdr evidence and one superseded TODO and no blocker | When the supervisor decides COMPLETE | Then completion is allowed and the goal stays closed so nudging stops", async () => {
  const done = [
    pendingTodo({ status: "done", evidence: [{ kind: "herdr_exec", ref: "exec:build-1" }] }),
    { id: "t2", title: "\u8dd1\u901a\u6d4b\u8bd5", kind: "test", status: "done", owner: "herdr_agent:pi", evidence: [{ kind: "herdr_test", ref: "exec:test-9" }] },
    { id: "t3", title: "\u65e7\u65b9\u6848", kind: "code", status: "superseded", superseded_reason: "merged into t1" },
  ];
  const { adapter } = adapterFor([{ decision: "COMPLETE", reason: "all TODOs have local evidence" }]);
  const result = await supervise({
    ledger: ledger(undefined, { checkpoint: { goal_supervisor: { objective: OBJ, todos: done } } }),
    boundaryEvent: { type: "turn_settled" },
    runtime: { ...healthyRuntime, observed_evidence_refs: ["exec:build-1", "exec:test-9"] },
    adapter,
    now: 10_000,
  });

  assert.equal(result.decision, "COMPLETE");
  assert.equal(result.effects.complete, true);
  assert.equal(result.effects.stop_nudging, true);
  assert.equal(result.ledger.closed, true);
  assert.equal(openTodos(result.ledger).length, 0);
});

// ---------------------------------------------------------------------------
// E. recovery is bounded and uncertainty is reconcile-only
// ---------------------------------------------------------------------------

test("user is not double-charged by a blind replay | Given an uncertain browser mutation | When the supervisor proposes RECOVER with retry_class uncertain | Then no message is sent and reconciliation is required instead", async () => {
  const { adapter } = adapterFor([{
    decision: "RECOVER",
    reason: "browser lost the tab mid-submit",
    retry_class: "uncertain",
    message_to_send: "\u91cd\u65b0\u53d1\u9001\u521a\u624d\u90a3\u6761\u6d88\u606f",
  }]);
  const result = await supervise({
    ledger: ledger(),
    boundaryEvent: { type: "browser_loss", reason: "tab_closed" },
    runtime: { ...healthyRuntime, delivery_uncertain: true, browser_online: false },
    adapter,
    now: 10_000,
  });

  assert.equal(result.decision, "RECOVER");
  assert.equal(result.effects.send, null);
  assert.equal(result.effects.reconcile.required, true);
  assert.equal(result.effects.reconcile.replay_forbidden, true);
});

test("user is not stuck in a retry loop | Given the recovery budget, then the continue budget, already spent | When the supervisor proposes RECOVER then CONTINUE | Then both are refused and escalated to the human", async () => {
  const spend = (retry) => ledger(undefined, { checkpoint: { goal_supervisor: { objective: OBJ, todos: [pendingTodo()], retry } } });
  const recover = await supervise({
    ledger: spend({ continue: { used: 0, max: 6 }, recover: { used: 2, max: 2 } }),
    boundaryEvent: { type: "provider_error", reason: "timeout" },
    runtime: { ...healthyRuntime, provider_error: "timeout" },
    adapter: adapterFor([{ decision: "RECOVER", reason: "try again", retry_class: "provider", message_to_send: "\u91cd\u8bd5" }]).adapter,
    now: 10_000,
  });
  assert.equal(recover.reason, "retry_budget_exhausted");
  assert.equal(recover.safeDecision, "ASK_HUMAN");

  const cont = await supervise({
    ledger: spend({ continue: { used: 6, max: 6 }, recover: { used: 0, max: 2 } }),
    boundaryEvent: { type: "turn_settled" },
    runtime: healthyRuntime,
    adapter: adapterFor([{ decision: "CONTINUE", reason: "keep going", retry_class: "transient", message_to_send: "\u7ee7\u7eed" }]).adapter,
    now: 10_000,
  });
  assert.equal(cont.reason, "retry_budget_exhausted");
  assert.equal(cont.safeDecision, "ASK_HUMAN");
  assert.equal(cont.effects, null);
});

test("user keeps a bounded recovery path when the judgement service is down | Given a provider timeout, with a fresh then an exhausted recovery budget | When the transport recovery is planned | Then the first spends one budgeted RECOVER and the second escalates to the human", () => {
  const fresh = planTransportRecovery({ ledger: ledger(), runtime: { ...healthyRuntime, provider_error: "timeout" }, reason: "timeout", now: 10_000 });
  assert.equal(fresh.allowed, true);
  assert.equal(fresh.decision, "RECOVER");
  assert.equal(fresh.effects.send, null);
  assert.equal(fresh.ledger.retry.recover.used, 1);
  assert.equal(fresh.ledger.supervisor.last_boundary, "provider_error");

  const spent = planTransportRecovery({
    ledger: ledger(undefined, { checkpoint: { goal_supervisor: { objective: OBJ, todos: [pendingTodo()], retry: { recover: { used: 2, max: 2 } } } } }),
    runtime: { ...healthyRuntime, provider_error: "timeout" },
    reason: "timeout",
    now: 10_000,
  });
  assert.equal(spent.allowed, false);
  assert.equal(spent.decision, "ASK_HUMAN");
  assert.equal(spent.effects, null);
});

// ---------------------------------------------------------------------------
// F. handoff at the context threshold
// ---------------------------------------------------------------------------

test("user keeps working across a long conversation and is not rolled over early | Given the rollover threshold, a healthy conversation, and an in-flight transfer | When the supervisor decides HANDOFF each time | Then only the threshold case is authorised", async () => {
  const proposal = { decision: "HANDOFF", reason: "context pressure" };

  const atThreshold = await supervise({
    ledger: ledger(),
    boundaryEvent: { type: "context_threshold", reason: "estimated_text_tokens:81000" },
    runtime: { ...healthyRuntime, context_state: "rollover_required" },
    adapter: adapterFor([proposal]).adapter,
    now: 10_000,
  });
  assert.equal(atThreshold.decision, "HANDOFF");
  assert.equal(atThreshold.effects.send, null);
  assert.equal(atThreshold.effects.handoff.boundary, "context_threshold");

  const healthy = await supervise({
    ledger: ledger(), boundaryEvent: { type: "turn_settled" }, runtime: healthyRuntime,
    adapter: adapterFor([proposal]).adapter, now: 10_000,
  });
  assert.equal(healthy.status, "guard_denied");
  assert.equal(healthy.reason, "context_below_handoff_threshold");

  const inFlight = await supervise({
    ledger: ledger(),
    boundaryEvent: { type: "handoff_settled", reason: "handoff_done" },
    runtime: { ...healthyRuntime, context_state: "high_risk", handoff_active: true },
    adapter: adapterFor([proposal]).adapter,
    now: 10_000,
  });
  assert.equal(inFlight.reason, "handoff_in_flight");
  assert.equal(inFlight.safeDecision, "WAIT_EXTERNAL");
});

// ---------------------------------------------------------------------------
// G. bounded input, strict contract, meaningful boundaries only
// ---------------------------------------------------------------------------

test("user supervisor never receives the whole transcript or tool bodies | Given twenty turns including a raw tool payload and a very long turn | When the bounded input is built | Then it is capped, the tool body is dropped, and the long turn is truncated", () => {
  const turns = Array.from({ length: 20 }, (_, i) => ({ role: "assistant", text: `turn ${i} `.repeat(40) }));
  turns.push({ role: "tool", text: "{\"tool\":\"herdr_exec\"}" });
  turns.push({ role: "assistant", text: `{"tool":"herdr_exec","args":{"command":"${"x".repeat(2000)}"}}` });
  turns.push({ role: "user", text: `START-${"y".repeat(5000)}-ENDTAIL` });

  const input = buildSupervisorInput({
    ledger: ledger(),
    authoredTurns: turns,
    evidence: Array.from({ length: 40 }, (_, i) => ({ kind: "herdr_exec", ref: `exec:${i}`, summary: "z".repeat(400) })),
    runtime: healthyRuntime,
    boundary: { boundary: "turn_settled", reason: "settled" },
    now: 10_000,
  });

  assert.ok(input.text.length <= SUPERVISOR_LIMITS.totalInputChars);
  assert.equal(input.text.includes("\"tool\""), false);
  assert.equal(input.text.includes("ENDTAIL"), false);
  assert.ok(input.dropped.turns > 0);
  const turnSection = input.sections.find((s) => s.name === "recent_authored_turns");
  assert.ok(turnSection.body.split("\n").length <= SUPERVISOR_LIMITS.authoredTurns);
});

test("user is not charged an LLM call for a meaningless boundary | Given agent output, a working heartbeat, a repeated settle, and an unknown event | When the supervisor is offered those observations | Then it never runs", async () => {
  const { adapter, calls } = adapterFor([{ decision: "CONTINUE", reason: "x", message_to_send: "y" }]);
  for (const event of [
    { type: "agent_output", status: "working" },
    { type: "agent_working", status: "working" },
    { type: "agent_settled", status: "done", previous_status: "done" },
    { type: "something_else" },
  ]) {
    const result = await supervise({ ledger: ledger(), boundaryEvent: event, runtime: healthyRuntime, adapter, now: 20_000 });
    assert.equal(result.status, "not_run", JSON.stringify(event));
    assert.equal(result.llm_calls, 0);
  }
  assert.equal(calls.length, 0);
});

test("user is neither nagged twice in a row nor supervised without a goal | Given a run 3 seconds ago, then a goal-less conversation | When the settled boundary arrives | Then both are skipped without a provider call", async () => {
  const recent = ledger(undefined, {
    checkpoint: { goal_supervisor: { objective: OBJ, todos: [pendingTodo()], supervisor: { last_run_at: 9_000, last_boundary: "turn_settled" } } },
  });
  const { adapter, calls } = adapterFor([{ decision: "CONTINUE", reason: "x", message_to_send: "y" }]);
  const throttled = await supervise({ ledger: recent, boundaryEvent: { type: "turn_settled" }, runtime: healthyRuntime, adapter, now: 12_000 });
  assert.equal(throttled.status, "not_run");
  assert.equal(throttled.reason, "boundary_throttled");

  const noGoal = await supervise({ ledger: deriveGoalLedger({ authoredTurns: [] }), boundaryEvent: { type: "turn_settled" }, runtime: healthyRuntime, adapter, now: 12_000 });
  assert.equal(noGoal.status, "not_run");
  assert.equal(noGoal.reason, "no_objective");
  assert.equal(calls.length, 0);

  // A real error inside the throttle window is still supervised.
  const urgent = await supervise({ ledger: recent, boundaryEvent: { type: "provider_error", reason: "timeout" }, runtime: healthyRuntime, adapter, now: 12_000 });
  assert.notEqual(urgent.status, "not_run");
});

test("user never receives an action from a malformed model answer | Given prose, an unknown decision, an unknown field, a missing required field, or an over-long message | When each is parsed | Then none produces an executable decision and the runtime reports failure", async () => {
  const cases = [
    "I think we should keep going.",
    "{\"decision\":\"PROCEED\",\"reason\":\"x\"}",
    "{\"decision\":\"CONTINUE\",\"reason\":\"x\",\"message_to_send\":\"y\",\"extra\":1}",
    "{\"decision\":\"CONTINUE\",\"reason\":\"x\",\"message_to_send\":\"y\",\"retry_class\":\"wild\"}",
    "{\"decision\":\"WAIT_EXTERNAL\",\"reason\":\"waiting\"}",
    "{\"decision\":\"ANSWER_DECISION\",\"reason\":\"answer\",\"message_to_send\":\"A\"}",
    `{"decision":"CONTINUE","reason":"x","message_to_send":"${"m".repeat(SUPERVISOR_LIMITS.message + 5)}"}`,
    "{\"decision\":\"CONTINUE\"}",
  ];
  for (const raw of cases) assert.equal(parseSupervisorDecision(raw).ok, false, raw.slice(0, 60));
  assert.equal(parseSupervisorDecision("```json\n{\"decision\":\"CONTINUE\",\"reason\":\"open todo\",\"message_to_send\":\"go\"}\n```").ok, true);

  const { adapter, calls } = adapterFor(["not json at all", "still not json"]);
  const result = await supervise({ ledger: ledger(), boundaryEvent: { type: "turn_settled" }, runtime: healthyRuntime, adapter, now: 10_000 });
  assert.equal(result.status, "provider_failed");
  assert.equal(result.effects, null);
  assert.ok(calls.length <= DEFAULT_SUPERVISOR_POLICY.maxDecisionAttempts);
});

test("user is not supervised at all without a configured provider | Given no LLM configuration | When the supervisor runs | Then no transport call is made", async () => {
  const calls = [];
  const adapter = createBoundedSupervisorAdapter({
    request: async (payload) => { calls.push(payload); return { ok: true, content: "{}" }; },
    isConfigured: () => false,
  });
  const result = await supervise({ ledger: ledger(), boundaryEvent: { type: "turn_settled" }, runtime: healthyRuntime, adapter, now: 10_000 });
  assert.equal(result.status, "provider_failed");
  assert.equal(result.reason, "not_configured");
  assert.equal(calls.length, 0);
});

// ---------------------------------------------------------------------------
// H. the ledger is derived, not stored
// ---------------------------------------------------------------------------

test("user goal is read from the existing Work Memory checkpoint first | Given a checkpoint carrying a goal section and different turn text | When the ledger is derived | Then the checkpoint wins and is marked as its source", () => {
  const fromCheckpoint = deriveGoalLedger({
    continuityId: "hc:1",
    checkpoint: {
      goal_supervisor: {
        objective: "\u6765\u81ea checkpoint \u7684\u76ee\u6807",
        human_constraints: ["\u4e0d\u5f97\u91cd\u547d\u540d\u8bbe\u5907"],
        todos: [{ id: "c1", title: "checkpoint todo", kind: "test", status: "pending" }],
      },
    },
    authoredTurns: [{ role: "user", text: "\u6765\u81ea\u4f1a\u8bdd\u7684\u6307\u4ee4" }],
    now: 5,
  });
  assert.equal(fromCheckpoint.source, "work_memory_checkpoint");
  assert.equal(fromCheckpoint.objective, "\u6765\u81ea checkpoint \u7684\u76ee\u6807");
  assert.equal(fromCheckpoint.todos[0].id, "c1");
  assert.equal(fromCheckpoint.continuity_id, "hc:1");
  assert.equal(goalLedgerFromCheckpoint({ other: 1 }), null);
});

test("user goal falls back to the continuity-anchored conversation | Given no checkpoint goal section and a user instruction with declared remaining work | When the ledger is derived | Then the objective is the user's own instruction and every derived TODO starts pending", () => {
  const derived = deriveGoalLedger({
    continuityId: "hc:2",
    authoredTurns: [{ role: "user", text: OBJ }],
    declaredRemaining: [{ title: "finish the build script" }, { title: "run the tests", kind: "test" }],
    now: 5,
  });
  assert.equal(derived.source, "continuity_turns");
  assert.equal(derived.objective, OBJ);
  assert.equal(derived.todos.length, 2);
  assert.ok(derived.todos.every((t) => t.status === "pending" && t.kind !== "human"));
  assert.equal(goalLedgerComplete(derived).complete, false);
});

test("user derived ledger cannot be grown or closed by a model answer | Given a model answer with forty TODOs and a done claim | When the updates are applied | Then the ledger stays bounded and nothing is closed without evidence", () => {
  const updates = Array.from({ length: 40 }, (_, i) => ({ id: `t${i}`, status: "done", title: "T".repeat(2000) }));
  const applied = applyTodoUpdates(ledger(Array.from({ length: 40 }, (_, i) => pendingTodo({ id: `t${i}` }))), updates, 5);
  assert.ok(applied.ledger.todos.length <= SUPERVISOR_LIMITS.todos);
  assert.ok(applied.ledger.todos.every((todo) => todo.status === "pending"));
  for (const todo of applied.ledger.todos) assert.ok(todo.title.length <= SUPERVISOR_LIMITS.todoTitle);
});

// ---------------------------------------------------------------------------
// I. guard surface
// ---------------------------------------------------------------------------

test("user guard always explains a refusal in a form code can act on | Given a denied proposal for an incomplete goal, an invalid proposal, and a still-generating turn | When the guard returns | Then each carries a reason, a safe state, and no executable send", () => {
  const proposals = [
    [{ ok: true, decision: "COMPLETE", reason: "done", retry_class: "none", todo_updates: [], message_to_send: null, choice: null, derived_from: null, external_owner: null }, healthyRuntime],
    [{ ok: false, error: "unknown_decision" }, healthyRuntime],
    [{ ok: true, decision: "CONTINUE", reason: "go", retry_class: "none", todo_updates: [], message_to_send: "go", choice: null, derived_from: null, external_owner: null }, { ...healthyRuntime, generation_settled: false }],
  ];
  for (const [proposal, runtime] of proposals) {
    const guard = guardSupervisorDecision(proposal, { runtime, ledger: ledger() });
    assert.equal(guard.allowed, false);
    assert.ok(String(guard.reason).length > 0);
    assert.equal(SUPERVISOR_DECISIONS.includes(guard.safeDecision), true);
    assert.equal(guard.effects, null);
  }
});

test("user is never sent a message that performs a human-boundary action | Given a CONTINUE whose text would delete, force-push or pay | When the guard inspects the message width | Then nothing is sent", () => {
  for (const message of ["\u5df2\u5220\u9664\u65e7 migration \u76ee\u5f55\u3002", "\u5df2 force push \u5230 remote\u3002", "\u5df2\u8d2d\u4e70\u4f01\u4e1a\u7248\u3002"]) {
    const guard = guardSupervisorDecision({
      ok: true, decision: "CONTINUE", reason: "go", retry_class: "none",
      todo_updates: [], message_to_send: message, choice: null, derived_from: null, external_owner: null,
    }, { runtime: healthyRuntime, ledger: ledger() });
    assert.equal(guard.allowed, false, message);
    assert.equal(guard.safeDecision, "ASK_HUMAN");
  }
});
// ---------------------------------------------------------------------------
// J. no double-send + authoritative Work Memory locator + CAS reconciliation
// ---------------------------------------------------------------------------

test("user is never double-sent on a supervisor-owned boundary | Given a settled boundary the supervisor already claimed with its assistant fingerprint | When the same assistant settles again | Then the boundary claim refuses a second decision", () => {
  const claims = new Map();
  const first = claimSupervisorBoundary(claims, "conv-1", "fp-123");
  const second = claimSupervisorBoundary(claims, "conv-1", "fp-123");
  const other = claimSupervisorBoundary(claims, "conv-2", "fp-456");
  assert.equal(first.claimed, true);
  assert.equal(second.claimed, false);
  assert.equal(second.reason, "boundary_already_handled");
  assert.equal(other.claimed, true);
});

test("user goal is only resumed from a unique exact Work Memory locator | Given a continuity.search candidate that is unique_exact and matches the binding, one that is ambiguous, and one whose continuity mismatches | When each is resolved | Then only the unique exact match yields a locator", () => {
  const good = resolveAuthoritativeWorkMemoryLocator({
    continuity_id: "hc:abc",
    work_memory: { project_ref: "proj", repo_id: "repo", work_chain_id: "wc_1" },
  }, "hc:abc");
  assert.equal(good.ok, true);
  assert.deepEqual(good.locator, { project_ref: "proj", repo_id: "repo", work_chain_id: "wc_1" });

  const mismatched = resolveAuthoritativeWorkMemoryLocator({
    continuity_id: "hc:other",
    work_memory: { project_ref: "proj", repo_id: "repo", work_chain_id: "wc_1" },
  }, "hc:abc");
  assert.equal(mismatched.ok, false);
  assert.equal(mismatched.reason, "locator_continuity_mismatch");

  assert.equal(resolveAuthoritativeWorkMemoryLocator({ continuity_id: "hc:x" }, "hc:x").ok, false);
  assert.equal(resolveAuthoritativeWorkMemoryLocator({
    continuity_id: "hc:x",
    work_memory: { project_ref: "proj" },
  }, "hc:x").ok, false);
});

test("user goal carries its objective, human constraints and accepted decisions across a checkpoint round-trip | Given a ledger with a constraint and an accepted decision | When it is merged into a checkpoint and read back | Then the goal section round-trips fully", () => {
  const goal = normalizeGoalLedger({
    objective: OBJ,
    human_constraints: ["\u4e0d\u5f97\u6539\u52a8\u751f\u4ea7\u914d\u7f6e"],
    accepted_decisions: [{ id: "d1", decision: "\u7528 A", derived_from: ["constraint:0"] }],
    todos: [{ id: "t1", title: "build", kind: "code", status: "working" }],
  });
  const existing = { owner: "another-writer", other: "preserved" };
  const merged = mergeGoalCheckpoint(existing, goal);
  assert.equal(merged.other, "preserved");
  assert.equal(merged.owner, "another-writer");
  const restored = goalLedgerFromCheckpoint(merged);
  assert.equal(restored.objective, OBJ);
  assert.deepEqual(restored.human_constraints, ["\u4e0d\u5f97\u6539\u52a8\u751f\u4ea7\u914d\u7f6e"]);
  assert.equal(restored.accepted_decisions[0].id, "d1");
  assert.equal(restored.todos[0].status, "working");
});

test("user checkpoint is never overwritten on a CAS conflict | Given an expected revision that no longer matches the authoritative checkpoint | When the put error is classified | Then it is a reconciliation-retry, never a last-write-wins, and a missing turn or missing evidence stays failed closed", () => {
  const conflict = classifyCheckpointPutError("work_memory_checkpoint_revision_conflict:7");
  assert.equal(conflict.kind, "revision_conflict");
  assert.equal(conflict.revision, 7);
  assert.equal(conflict.retry, true);
  assert.equal(classifyCheckpointPutError("work_memory_not_found").kind, "not_found");
  assert.equal(classifyCheckpointPutError("work_memory_checkpoint_turn_not_found").kind, "turn_missing");
  assert.equal(classifyCheckpointPutError("work_memory_checkpoint_evidence_not_found").kind, "evidence_missing");
  assert.equal(classifyCheckpointPutError("something else").kind, "other");
});

