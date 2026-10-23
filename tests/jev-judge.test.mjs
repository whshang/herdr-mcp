import test from "node:test";
import assert from "node:assert/strict";

import {
  DEFAULT_JEV_THRESHOLD,
  JEV_GOAL_SIGNAL_KEYS,
  buildJevGoalSemanticRequest,
  buildJevPendingWorkRequest,
  decideJevAutoPolicy,
  interpretJevGoalSemanticAnswer,
  interpretJevPendingWorkAnswer,
} from "../extension/jev-judge-core.js";

test("user gets one narrow Jev question | Given a settled turn | When the semantic gate request is built | Then only bounded user and assistant state are sent", () => {
  const request = buildJevPendingWorkRequest(
    "Please finish the implementation and verify it.",
    "Implementation is done. Next: I will run the production verification.",
  );
  assert.equal(Object.prototype.hasOwnProperty.call(request, "model"), false);
  assert.deepEqual(Object.keys(request.questions), [
    "has_unfinished_work",
    "handoff_boundary_stable",
    "useful_work_remaining",
  ]);
  assert.equal(request.questions.has_unfinished_work.type, "noul");
  assert.equal(request.questions.handoff_boundary_stable.type, "noul");
  assert.equal(request.questions.useful_work_remaining.type, "noul");
  assert.match(request.questions.has_unfinished_work.instructions, /assistant itself can continue/i);
  assert.match(request.questions.has_unfinished_work.criteria.false, /requires a user\/external decision/i);
});

test("user gets the fixed Jev tri-state boundary | Given positive negative and middle probabilities | When Jev is interpreted | Then 0.80 and 0.20 bound true false and uncertainty", () => {
  const answer = (noul) => ({ model: "jev-latest", answers: { has_unfinished_work: { type: "noul", noul } } });
  assert.equal(DEFAULT_JEV_THRESHOLD, 0.80);
  assert.equal(interpretJevPendingWorkAnswer(answer(0.80)).result, "true");
  assert.equal(interpretJevPendingWorkAnswer(answer(0.80)).signal, "continue");
  assert.equal(interpretJevPendingWorkAnswer(answer(0.20)).result, "false");
  assert.equal(interpretJevPendingWorkAnswer(answer(0.20)).signal, "done");
  assert.equal(interpretJevPendingWorkAnswer(answer(0.55)).result, "uncertain");
  assert.equal(interpretJevPendingWorkAnswer(answer(0.55)).signal, "uncertain");
  assert.equal(interpretJevPendingWorkAnswer(answer(2)).ok, false);

  const advisory = interpretJevPendingWorkAnswer({
    model: "jev-latest",
    answers: {
      has_unfinished_work: { type: "noul", noul: 0.55 },
      handoff_boundary_stable: { type: "noul", noul: 0.82 },
      useful_work_remaining: { type: "noul", noul: 0.64 },
    },
  });
  assert.equal(advisory.handoffBoundaryStable, 0.82);
  assert.equal(advisory.usefulWorkRemaining, 0.64);
  assert.equal(interpretJevPendingWorkAnswer(answer(0.55)).handoffBoundaryStable, null);
});

test("user never gets a stop decision from malformed provider output | Given a Jev pending-work reply whose probability is null empty non-numeric or a string | When the pending-work answer is interpreted | Then every non-number fails closed to bad_response instead of a done signal", () => {
  const answer = (noul) => ({ model: "jev-latest", answers: { has_unfinished_work: { type: "noul", noul } } });
  for (const malformed of [null, "", false, true, [], {}, "0.9", "1", Number.NaN, Number.POSITIVE_INFINITY, -0.1, 1.1]) {
    const parsed = interpretJevPendingWorkAnswer(answer(malformed));
    assert.equal(parsed.ok, false, `noul=${JSON.stringify(malformed)} must not be accepted`);
    assert.equal(parsed.reason, "bad_response");
    assert.equal(parsed.signal, "unknown");
    assert.equal(parsed.probability, null);
    assert.deepEqual(decideJevAutoPolicy(parsed), { action: "fallback", reason: "jev_unavailable" });
  }
  // A valid number still behaves exactly as before, including the 0.20 false boundary.
  assert.equal(interpretJevPendingWorkAnswer(answer(0.2)).signal, "done");
  assert.equal(interpretJevPendingWorkAnswer(answer(0)).signal, "done");
});

test("user never gets an all-false Goal prior from malformed provider output | Given a Jev Goal reply whose five signals are null or strings | When the Goal prior is interpreted | Then it fails closed to bad_response instead of reporting every signal as false", () => {
  const answers = (noul) => Object.fromEntries(JEV_GOAL_SIGNAL_KEYS.map((key) => [key, { type: "noul", noul }]));
  for (const malformed of [null, "", "0.9", false, []]) {
    const parsed = interpretJevGoalSemanticAnswer({ answers: answers(malformed) });
    assert.equal(parsed.ok, false, `noul=${JSON.stringify(malformed)} must not be accepted`);
    assert.equal(parsed.reason, "bad_response");
    assert.equal(parsed.probabilities, null);
  }
  // One malformed optional head is not a probability hint, but it never becomes a decision.
  const mixed = interpretJevPendingWorkAnswer({
    answers: {
      has_unfinished_work: { type: "noul", noul: 0.55 },
      handoff_boundary_stable: { type: "noul", noul: null },
      useful_work_remaining: { type: "noul", noul: "0.64" },
    },
  });
  assert.equal(mixed.ok, true);
  assert.equal(mixed.handoffBoundaryStable, null);
  assert.equal(mixed.usefulWorkRemaining, null);

  const goal = interpretJevGoalSemanticAnswer({
    answers: { ...answers(0.05), waiting_external: { type: "noul", noul: 0.92 } },
  });
  assert.equal(goal.ok, true);
  assert.deepEqual(goal.strong, ["waiting_external"]);
});

test("user gets Jev first and fallback only on uncertainty or outage | Given explicit Jev outcomes | When Auto selects the semantic stage | Then continue and done are final while uncertain or unavailable fall through", () => {
  assert.deepEqual(
    decideJevAutoPolicy({ ok: true, signal: "continue", probability: 0.91 }),
    { action: "continue", reason: "jev_continue" },
  );
  assert.deepEqual(
    decideJevAutoPolicy({ ok: true, signal: "done", probability: 0.08 }),
    { action: "stop", reason: "jev_done" },
  );
  assert.deepEqual(
    decideJevAutoPolicy({ ok: true, signal: "uncertain", probability: 0.51 }),
    { action: "fallback", reason: "jev_uncertain" },
  );
  assert.deepEqual(
    decideJevAutoPolicy({ ok: false, reason: "timeout" }),
    { action: "fallback", reason: "jev_unavailable" },
  );
});

test("user gets one bounded Jev Goal classification | Given a goal boundary | When the semantic prior is requested | Then the five fixed Noul signals are evaluated together", () => {
  const request = buildJevGoalSemanticRequest({
    objective: "Finish the release.",
    userText: "Continue until everything is done.",
    assistantText: "Tests pass, CI is still running.",
    openTodos: ["verify CI", "publish after approval"],
    boundary: "turn_settled",
  });
  assert.deepEqual(Object.keys(request.questions), [...JEV_GOAL_SIGNAL_KEYS]);
  assert.equal(request.questions.can_continue.type, "noul");
  assert.equal(request.questions.needs_human.type, "noul");
  assert.equal(request.questions.waiting_external.type, "noul");
  assert.equal(request.questions.task_completed.type, "noul");
  assert.equal(request.questions.needs_handoff.type, "noul");
  for (const key of JEV_GOAL_SIGNAL_KEYS) {
    assert.equal(typeof request.questions[key].criteria?.true, "string");
    assert.equal(typeof request.questions[key].criteria?.false, "string");
    assert.ok(request.questions[key].criteria.true.length > 0);
    assert.ok(request.questions[key].criteria.false.length > 0);
  }
});

test("user gets probability hints rather than a second Goal authority | Given a complete five-signal Jev response | When it is interpreted | Then probabilities and strong signals are returned without an executable decision", () => {
  const answers = Object.fromEntries(JEV_GOAL_SIGNAL_KEYS.map((key) => [key, { type: "noul", noul: 0.1 }]));
  answers.waiting_external.noul = 0.92;
  const result = interpretJevGoalSemanticAnswer({ model: "jev-latest", answers });
  assert.equal(result.ok, true);
  assert.equal(result.probabilities.waiting_external, 0.92);
  assert.deepEqual(result.strong, ["waiting_external"]);
  assert.equal(Object.prototype.hasOwnProperty.call(result, "decision"), false);
});
