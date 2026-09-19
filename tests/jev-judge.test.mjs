import test from "node:test";
import assert from "node:assert/strict";

import {
  DEFAULT_JEV_THRESHOLD,
  JEV_JUDGE_MODE_ASSIST,
  assistLlmVerdictWithJev,
  buildJevPendingWorkRequest,
  interpretJevPendingWorkAnswer,
  jevAgreementWithLlm,
  jevSystemOneUrl,
  normalizeJevJudgeMode,
  normalizeJevJudgeThreshold,
} from "../extension/jev-judge-core.js";

test("user gets a narrow Jev pending-work question | Given a settled user and assistant turn | When the auxiliary request is built | Then only bounded task state and one Noul judgment are sent", () => {
  const request = buildJevPendingWorkRequest(
    "Please finish the implementation and verify it.",
    "Implementation is done. Next: I will run the production verification.",
    "jev-latest",
  );
  assert.equal(request.model, "jev-latest");
  assert.deepEqual(Object.keys(request.questions), ["has_unfinished_work"]);
  assert.equal(request.questions.has_unfinished_work.type, "noul");
  assert.match(request.questions.has_unfinished_work.instructions, /assistant itself can continue/i);
  assert.match(request.questions.has_unfinished_work.criteria.false, /requires a user\/external decision/i);
  assert.equal(request.state.user_request, "Please finish the implementation and verify it.");
});

test("user gets calibrated Jev signals | Given high yes, high no, and middle probabilities | When the response is interpreted | Then continue, done, and uncertain stay distinct", () => {
  const answer = (noul) => ({ model: "jev-latest", answers: { has_unfinished_work: { type: "noul", noul } } });
  assert.equal(interpretJevPendingWorkAnswer(answer(0.91), 0.8).signal, "continue");
  assert.equal(interpretJevPendingWorkAnswer(answer(0.09), 0.8).signal, "done");
  assert.equal(interpretJevPendingWorkAnswer(answer(0.55), 0.8).signal, "uncertain");
  assert.equal(interpretJevPendingWorkAnswer(answer(2), 0.8).ok, false);
});

test("user keeps the existing LLM continuation | Given the LLM already says continue and Jev disagrees | When assist mode composes the judgments | Then Jev cannot suppress the existing continue", () => {
  const result = assistLlmVerdictWithJev(
    { done: false, cont: true, nudgeText: "Continue.", raw: "Continue." },
    { ok: true, signal: "done", probability: 0.05 },
    "Continue with unfinished work.",
  );
  assert.equal(result.assisted, false);
  assert.equal(result.verdict.cont, true);
  assert.equal(jevAgreementWithLlm(result.verdict, { ok: true, signal: "done" }), "disagree_llm_continue");
});

test("user recovers a false done judgment | Given the LLM says done but Jev strongly sees autonomous unfinished work | When assist mode composes the judgments | Then it changes only that missed continuation into a bounded nudge", () => {
  const result = assistLlmVerdictWithJev(
    { done: true, cont: false, nudgeText: "", raw: "DONE" },
    { ok: true, signal: "continue", probability: 0.94 },
    "Continue with the unfinished work you identified.",
  );
  assert.equal(result.assisted, true);
  assert.equal(result.verdict.done, false);
  assert.equal(result.verdict.cont, true);
  assert.equal(result.verdict.nudgeText, "Continue with the unfinished work you identified.");
  assert.match(result.verdict.raw, /jev_assist p=0\.940/);
});

test("user is not auto-continued by uncertain Jev evidence | Given the existing LLM is done or ambiguous and Jev is below the configured confidence boundary | When judgments are composed | Then the original result remains unchanged", () => {
  for (const verdict of [
    { done: true, cont: false, raw: "DONE" },
    { done: false, cont: false, raw: "maybe" },
  ]) {
    const result = assistLlmVerdictWithJev(
      verdict,
      { ok: true, signal: "uncertain", probability: 0.61 },
      "Continue.",
    );
    assert.equal(result.assisted, false);
    assert.equal(result.verdict.cont, false);
  }
});

test("user can configure the TypeSafe endpoint without hiding provider choices | Given a base URL, mode, and threshold | When settings are normalized | Then endpoint construction is explicit and invalid thresholds fail to the conservative default", () => {
  assert.equal(jevSystemOneUrl("https://api.typesafe.ai/v1"), "https://api.typesafe.ai/v1/systemone");
  assert.equal(jevSystemOneUrl("https://example.test/custom/systemone"), "https://example.test/custom/systemone");
  assert.equal(normalizeJevJudgeMode("assist"), JEV_JUDGE_MODE_ASSIST);
  assert.equal(normalizeJevJudgeMode("anything"), "off");
  assert.equal(normalizeJevJudgeThreshold("0.9"), 0.9);
  assert.equal(normalizeJevJudgeThreshold("0.2"), DEFAULT_JEV_THRESHOLD);
});
