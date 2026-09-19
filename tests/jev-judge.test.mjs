import test from "node:test";
import assert from "node:assert/strict";

import {
  DEFAULT_JEV_THRESHOLD,
  buildJevPendingWorkRequest,
  decideJevAutoPolicy,
  interpretJevPendingWorkAnswer,
  isJevJudgeConfigured,
  jevSystemOneUrl,
} from "../extension/jev-judge-core.js";
import {
  llmJudgeCompletionsUrl,
  validateApiBaseUrl,
} from "../extension/binding-core.js";

test("user gets one narrow Jev question | Given a settled turn | When the semantic gate request is built | Then only bounded user and assistant state are sent", () => {
  const request = buildJevPendingWorkRequest(
    "Please finish the implementation and verify it.",
    "Implementation is done. Next: I will run the production verification.",
  );
  assert.equal(request.model, "jev-latest");
  assert.deepEqual(Object.keys(request.questions), ["has_unfinished_work"]);
  assert.equal(request.questions.has_unfinished_work.type, "noul");
  assert.match(request.questions.has_unfinished_work.instructions, /assistant itself can continue/i);
  assert.match(request.questions.has_unfinished_work.criteria.false, /requires a user\/external decision/i);
});

test("user gets the calibrated fixed Jev boundary | Given continue done and middle probabilities | When Jev is interpreted | Then 0.70 separates continue done and fallback", () => {
  const answer = (noul) => ({ model: "jev-latest", answers: { has_unfinished_work: { type: "noul", noul } } });
  assert.equal(DEFAULT_JEV_THRESHOLD, 0.70);
  assert.equal(interpretJevPendingWorkAnswer(answer(0.70)).signal, "continue");
  assert.equal(interpretJevPendingWorkAnswer(answer(0.30)).signal, "done");
  assert.equal(interpretJevPendingWorkAnswer(answer(0.55)).signal, "uncertain");
  assert.equal(interpretJevPendingWorkAnswer(answer(2)).ok, false);
});

test("user automatically enables Jev from provider parameters | Given endpoint model and key are complete | When configuration is checked | Then no separate mode or threshold is required", () => {
  assert.equal(isJevJudgeConfigured({
    jevJudgeBaseUrl: "https://api.typesafe.ai/v1",
    jevJudgeApiKey: "secret",
    jevJudgeModel: "jev-latest",
  }), true);
  assert.equal(isJevJudgeConfigured({
    jevJudgeBaseUrl: "https://api.typesafe.ai/v1",
    jevJudgeApiKey: "",
    jevJudgeModel: "jev-latest",
  }), false);
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

test("user keeps provider URLs explicit | Given TypeSafe base URLs | When the System One URL is built | Then only the documented endpoint suffix is appended", () => {
  assert.equal(jevSystemOneUrl("https://api.typesafe.ai/v1"), "https://api.typesafe.ai/v1/systemone");
  assert.equal(jevSystemOneUrl("https://example.test/custom/systemone"), "https://example.test/custom/systemone");
});

test("user is warned about a mistyped LLM base URL | Given a duplicate path slash | When settings validate the URL | Then saving is rejected with a display-only suggestion and runtime does not silently repair it", () => {
  const bad = validateApiBaseUrl("https://cc.whshang.me//v1");
  assert.equal(bad.ok, false);
  assert.equal(bad.reason, "duplicate_path_slash");
  assert.equal(bad.suggestion, "https://cc.whshang.me/v1");
  assert.equal(llmJudgeCompletionsUrl("https://cc.whshang.me//v1"), "");
  assert.equal(llmJudgeCompletionsUrl("https://cc.whshang.me/v1"), "https://cc.whshang.me/v1/chat/completions");
});
