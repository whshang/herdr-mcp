#!/usr/bin/env node
// Deterministic semantic route benchmark cases.
//
// Every typed case payload is produced by the shipped prompt builder that the
// runtime/extension actually uses, so the benchmark measures the configured
// routes against production request shapes instead of a hand-written imitation:
//   - extension/jev-judge-core.js      buildJevPendingWorkRequest / buildJevGoalSemanticRequest
//   - crates/herdr-mcp/src/mcp.rs      browser_fast_path_decision head wording (mirrored, see below)
//   - extension/goal-supervisor-core.js SUPERVISOR_SYSTEM_PROMPT + buildSupervisorUserMessage
//   - extension/binding-core.js        buildLlmJudgeUserMessage
//
// The browser fast-path cases mirror the four heads/questions that
// `browser_fast_path_decision` asks in crates/herdr-mcp/src/mcp.rs. They are
// mirrored rather than imported because that controller is Rust-internal; the
// mirror is limited to the question wording documented in that function.
//
// Output: tests/bench/fixtures/semantic-cases.json (committed, byte-stable).
// Usage: `node scripts/bench/generate-semantic-cases.mjs [--check]`
//   --check  fail when the committed fixture is not byte-identical (reproducibility gate)

import { createHash } from "node:crypto";
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import {
  buildJevGoalSemanticRequest,
  buildJevPendingWorkRequest,
} from "../../extension/jev-judge-core.js";
import {
  SUPERVISOR_SYSTEM_PROMPT,
  buildSupervisorInput,
  buildSupervisorUserMessage,
} from "../../extension/goal-supervisor-core.js";
import { buildLlmJudgeUserMessage } from "../../extension/binding-core.js";

const here = dirname(fileURLToPath(import.meta.url));
const fixturePath = resolve(here, "../../tests/bench/fixtures/semantic-cases.json");

/** Deterministic key order so the fixture is byte-stable across runs. */
function canonical(value) {
  if (Array.isArray(value)) return value.map(canonical);
  if (value && typeof value === "object") {
    return Object.fromEntries(Object.keys(value).sort().map((key) => [key, canonical(value[key])]));
  }
  return value;
}

function typedCase(id, origin, payload, expect) {
  return {
    id,
    family: "typed",
    origin,
    payload: { state: payload.state, questions: payload.questions },
    expect,
  };
}

const PENDING_WORK_ORIGIN = "extension/jev-judge-core.js buildJevPendingWorkRequest";
const GOAL_ORIGIN = "extension/jev-judge-core.js buildJevGoalSemanticRequest";
const FAST_PATH_ORIGIN = "crates/herdr-mcp/src/mcp.rs browser_fast_path_decision (mirrored heads)";

function typedCases() {
  return [
    typedCase(
      "typed_pending_work_continue",
      PENDING_WORK_ORIGIN,
      buildJevPendingWorkRequest(
        "Please implement the retry helper and run the full test suite.",
        "The retry helper is implemented. Next: I will run the full test suite and fix any failures.",
      ),
      { has_unfinished_work: "true", useful_work_remaining: "true" },
    ),
    typedCase(
      "typed_pending_work_done",
      PENDING_WORK_ORIGIN,
      buildJevPendingWorkRequest(
        "Add the retry helper with tests.",
        "Done. The retry helper and its tests are committed, the suite passes, and nothing is left open. Let me know if you want the docs updated too.",
      ),
      { has_unfinished_work: "false", useful_work_remaining: "false" },
    ),
    typedCase(
      "typed_pending_work_human_decision",
      PENDING_WORK_ORIGIN,
      buildJevPendingWorkRequest(
        "Ship the release.",
        "Everything is prepared. I need your approval before publishing the production release; tell me when to continue.",
      ),
      { has_unfinished_work: "false", useful_work_remaining: "false" },
    ),
    typedCase(
      "typed_goal_can_continue",
      GOAL_ORIGIN,
      buildJevGoalSemanticRequest({
        objective: "Finish the v1.1 semantic benchmark.",
        userText: "Keep going until the benchmark report exists.",
        assistantText: "I collected the fixtures and will now run the per-route measurement.",
        openTodos: ["run per-route measurement", "write the results table"],
        boundary: "turn_settled",
      }),
      { can_continue: "true", needs_human: "false", task_completed: "false" },
    ),
    typedCase(
      "typed_goal_needs_human",
      GOAL_ORIGIN,
      buildJevGoalSemanticRequest({
        objective: "Publish the release with the correct license.",
        userText: "Pick whatever license you think is best and publish it.",
        assistantText: "Two licenses are plausible and the choice is a product/legal decision. Which one should I use?",
        openTodos: ["publish release"],
        boundary: "turn_settled",
      }),
      { needs_human: "true", can_continue: "false", task_completed: "false" },
    ),
    typedCase(
      "typed_goal_waiting_external",
      GOAL_ORIGIN,
      buildJevGoalSemanticRequest({
        objective: "Land the change once CI is green.",
        userText: "Continue until it is merged.",
        assistantText: "The pipeline is still running; I will wait for the CI result before doing anything else.",
        openTodos: ["collect CI result"],
        boundary: "turn_settled",
      }),
      { waiting_external: "true", needs_human: "false", task_completed: "false" },
    ),
    typedCase(
      "typed_goal_task_completed",
      GOAL_ORIGIN,
      buildJevGoalSemanticRequest({
        objective: "Add and verify the retry helper.",
        userText: "Add the retry helper.",
        assistantText: "All requested work is complete: the helper is implemented, tested and committed, and every open TODO is closed.",
        openTodos: [],
        boundary: "turn_settled",
      }),
      { task_completed: "true", can_continue: "false", waiting_external: "false" },
    ),
    typedCase(
      "typed_goal_needs_handoff",
      GOAL_ORIGIN,
      buildJevGoalSemanticRequest({
        objective: "Finish the long multi-stage migration.",
        userText: "Continue the migration.",
        assistantText: "The context window is nearly exhausted, so continuing this objective needs a fresh conversation with a continuity handoff.",
        openTodos: ["migrate remaining call sites"],
        boundary: "context_pressure",
        runtimeSummary: "context_state=rollover_required",
      }),
      { needs_handoff: "true", task_completed: "false" },
    ),
    typedCase(
      "typed_fastpath_objective_done",
      FAST_PATH_ORIGIN,
      {
        state: {
          objective: "Submit the support request and confirm it was accepted.",
          observation: {
            url: "https://example.test/support/thanks",
            origin: "https://example.test",
            title: "Request received",
            generation: "g1",
            text: "Thank you. Your request has been received and is confirmed. Reference 48213.",
            elements: [],
          },
          candidate_count: 0,
          authority: "advisory candidate selection only; no browser action is executed here",
        },
        questions: {
          objective_done: {
            type: "noul",
            instructions: "Does the current visible page itself prove that the objective is already satisfied?",
            criteria: {
              true: "The current page visibly proves the objective is satisfied",
              false: "The current page does not yet visibly prove the objective is satisfied",
            },
          },
          external_blocked: {
            type: "noul",
            instructions: "Does the current visible page require a human or external action before Herdr can safely continue autonomously? Treat login or sign-in, CAPTCHA or challenge, consent or approval, permission, payment, and waiting for an external result as blocked. Do not call the page blocked when the visible evidence allows an offered low-risk action to make autonomous progress.",
            criteria: {
              true: "A visible human or external boundary prevents autonomous progress",
              false: "No visible human or external boundary prevents autonomous progress",
            },
          },
        },
      },
      { objective_done: "true", external_blocked: "false" },
    ),
    typedCase(
      "typed_fastpath_login_boundary",
      FAST_PATH_ORIGIN,
      {
        state: {
          objective: "Open the billing report and read the totals.",
          observation: {
            url: "https://example.test/login",
            origin: "https://example.test",
            title: "Sign in",
            generation: "g2",
            text: "Sign in to continue. Enter your password to view the billing report.",
            elements: [],
          },
          candidate_count: 0,
          authority: "advisory candidate selection only; no browser action is executed here",
        },
        questions: {
          objective_done: {
            type: "noul",
            instructions: "Does the current visible page itself prove that the objective is already satisfied?",
            criteria: {
              true: "The current page visibly proves the objective is satisfied",
              false: "The current page does not yet visibly prove the objective is satisfied",
            },
          },
          external_blocked: {
            type: "noul",
            instructions: "Does the current visible page require a human or external action before Herdr can safely continue autonomously? Treat login or sign-in, CAPTCHA or challenge, consent or approval, permission, payment, and waiting for an external result as blocked. Do not call the page blocked when the visible evidence allows an offered low-risk action to make autonomous progress.",
            criteria: {
              true: "A visible human or external boundary prevents autonomous progress",
              false: "No visible human or external boundary prevents autonomous progress",
            },
          },
        },
      },
      { external_blocked: "true", objective_done: "false" },
    ),
    typedCase(
      "typed_fastpath_choose_primary_cta",
      FAST_PATH_ORIGIN,
      {
        state: {
          objective: "Continue the multi-step checkout form to the next step.",
          observation: {
            url: "https://example.test/wizard/step-1",
            origin: "https://example.test",
            title: "Wizard step 1 of 3",
            generation: "g3",
            text: "Step 1 of 3 completed. Choose Continue to go to step 2.",
            elements: [
              { candidate_id: "target_0", ref: "e10", class: "same_origin_navigation", text: "Continue" },
              { candidate_id: "target_1", ref: "e11", class: "reveal", text: "Help" },
              { candidate_id: "target_2", ref: "e12", class: "same_origin_navigation", text: "Learn more" },
            ],
          },
          candidate_count: 3,
          authority: "advisory candidate selection only; no browser action is executed here",
        },
        questions: {
          objective_done: {
            type: "noul",
            instructions: "Does the current visible page itself prove that the objective is already satisfied?",
            criteria: {
              true: "The current page visibly proves the objective is satisfied",
              false: "The current page does not yet visibly prove the objective is satisfied",
            },
          },
          external_blocked: {
            type: "noul",
            instructions: "Does the current visible page require a human or external action before Herdr can safely continue autonomously? Treat login or sign-in, CAPTCHA or challenge, consent or approval, permission, payment, and waiting for an external result as blocked. Do not call the page blocked when the visible evidence allows an offered low-risk action to make autonomous progress.",
            criteria: {
              true: "A visible human or external boundary prevents autonomous progress",
              false: "No visible human or external boundary prevents autonomous progress",
            },
          },
          safe_click_supported: {
            type: "noul",
            instructions: "Considering only the Runtime-offered low-risk click candidates shown in the current observation, is at least one candidate a reasonable next action that advances toward the stated objective? Evaluate progress, not final completion.",
            criteria: {
              true: "At least one offered candidate reasonably advances toward the objective",
              false: "None of the offered candidates reasonably advances toward the objective",
            },
          },
          click_target: {
            type: "choice",
            instructions: "If a low-risk click is supported, choose exactly one offered observed target whose visible label or semantics best match the requested next step. Never invent an element reference.",
            criteria: {
              target_0: "Click only this already-observed opaque element: {\"action\":\"click\",\"ref\":\"e10\",\"class\":\"same_origin_navigation\",\"label\":\"Continue\"}",
              target_1: "Click only this already-observed opaque element: {\"action\":\"click\",\"ref\":\"e11\",\"class\":\"reveal\",\"label\":\"Help\"}",
              target_2: "Click only this already-observed opaque element: {\"action\":\"click\",\"ref\":\"e12\",\"class\":\"same_origin_navigation\",\"label\":\"Learn more\"}",
            },
          },
        },
      },
      { objective_done: "false", external_blocked: "false", safe_click_supported: "true", click_target: "target_0" },
    ),
    typedCase(
      "typed_fastpath_choose_next_page",
      FAST_PATH_ORIGIN,
      {
        state: {
          objective: "Open the next page of archived notifications.",
          observation: {
            url: "https://example.test/archive?page=1",
            origin: "https://example.test",
            title: "Archive page 1",
            generation: "g4",
            text: "Archive page 1 of 12. Use the pager to read older entries.",
            elements: [
              { candidate_id: "target_0", ref: "e20", class: "reveal", text: "Help" },
              { candidate_id: "target_1", ref: "e21", class: "same_origin_navigation", text: "Next page" },
              { candidate_id: "target_2", ref: "e22", class: "same_origin_navigation", text: "Privacy policy" },
            ],
          },
          candidate_count: 3,
          authority: "advisory candidate selection only; no browser action is executed here",
        },
        questions: {
          objective_done: {
            type: "noul",
            instructions: "Does the current visible page itself prove that the objective is already satisfied?",
            criteria: {
              true: "The current page visibly proves the objective is satisfied",
              false: "The current page does not yet visibly prove the objective is satisfied",
            },
          },
          external_blocked: {
            type: "noul",
            instructions: "Does the current visible page require a human or external action before Herdr can safely continue autonomously? Treat login or sign-in, CAPTCHA or challenge, consent or approval, permission, payment, and waiting for an external result as blocked. Do not call the page blocked when the visible evidence allows an offered low-risk action to make autonomous progress.",
            criteria: {
              true: "A visible human or external boundary prevents autonomous progress",
              false: "No visible human or external boundary prevents autonomous progress",
            },
          },
          safe_click_supported: {
            type: "noul",
            instructions: "Considering only the Runtime-offered low-risk click candidates shown in the current observation, is at least one candidate a reasonable next action that advances toward the stated objective? Evaluate progress, not final completion.",
            criteria: {
              true: "At least one offered candidate reasonably advances toward the objective",
              false: "None of the offered candidates reasonably advances toward the objective",
            },
          },
          click_target: {
            type: "choice",
            instructions: "If a low-risk click is supported, choose exactly one offered observed target whose visible label or semantics best match the requested next step. Never invent an element reference.",
            criteria: {
              target_0: "Click only this already-observed opaque element: {\"action\":\"click\",\"ref\":\"e20\",\"class\":\"reveal\",\"label\":\"Help\"}",
              target_1: "Click only this already-observed opaque element: {\"action\":\"click\",\"ref\":\"e21\",\"class\":\"same_origin_navigation\",\"label\":\"Next page\"}",
              target_2: "Click only this already-observed opaque element: {\"action\":\"click\",\"ref\":\"e22\",\"class\":\"same_origin_navigation\",\"label\":\"Privacy policy\"}",
            },
          },
        },
      },
      { safe_click_supported: "true", click_target: "target_1" },
    ),
  ];
}

function supervisorMessages({ ledger, authoredTurns, runtime, boundary = null, semanticPrior = null }) {
  const input = buildSupervisorInput({ ledger, authoredTurns, runtime, boundary, semanticPrior });
  return [
    { role: "system", content: SUPERVISOR_SYSTEM_PROMPT },
    { role: "user", content: buildSupervisorUserMessage(input) },
  ];
}

function chatCase(id, origin, messages, expect) {
  return { id, family: "chat", origin, messages, expect };
}

function chatCases() {
  const supervisorOrigin = "extension/goal-supervisor-core.js SUPERVISOR_SYSTEM_PROMPT";
  const judgeOrigin = "extension/binding-core.js buildLlmJudgeUserMessage";
  return [
    chatCase(
      "chat_supervisor_continue",
      supervisorOrigin,
      supervisorMessages({
        ledger: {
          objective: "Finish the v1.1 semantic benchmark and commit the report.",
          todos: [{ id: "w1", title: "run the per-route measurement", status: "open", owner: "assistant", kind: "verify" }],
        },
        authoredTurns: [{ role: "user", at: 1, text: "Keep going until the benchmark report is committed." }],
        runtime: { agent_running: false, agent_status: "idle", context_state: "healthy" },
      }),
      { decision: "CONTINUE" },
    ),
    chatCase(
      "chat_supervisor_wait_external",
      supervisorOrigin,
      supervisorMessages({
        ledger: {
          objective: "Land the change once the local agent finishes the edit.",
          todos: [{ id: "w1", title: "collect the local agent result", status: "open", owner: "agent", kind: "verify" }],
        },
        authoredTurns: [{ role: "user", at: 2, text: "Continue until it is done." }],
        runtime: { agent_running: true, agent_status: "working", external_owner: "local agent", context_state: "healthy" },
      }),
      { decision: "WAIT_EXTERNAL" },
    ),
    chatCase(
      "chat_supervisor_complete",
      supervisorOrigin,
      supervisorMessages({
        ledger: {
          objective: "Add the retry helper to the export path.",
          todos: [{
            id: "w1",
            title: "add the retry helper to the export path",
            status: "done",
            owner: "assistant",
            kind: "implement",
            evidence: [{ kind: "herdr_agent", ref: "agent:w1:done:42" }],
          }],
        },
        authoredTurns: [{ role: "user", at: 3, text: "Add the retry helper to the export path and stop once it is in place." }],
        runtime: { agent_running: false, agent_status: "done", context_state: "healthy" },
      }),
      { decision: "COMPLETE" },
    ),
    chatCase(
      "chat_supervisor_ask_human",
      supervisorOrigin,
      supervisorMessages({
        ledger: {
          objective: "Publish the release under the license the user picks.",
          human_constraints: ["the release license is the user's own decision and cannot be delegated"],
          todos: [{ id: "w1", title: "publish under the user-selected license", status: "open", owner: "human", kind: "decide" }],
        },
        authoredTurns: [{ role: "user", at: 4, text: "I will pick the license myself later and I do not want a recommendation. Ask me before you publish anything." }],
        runtime: { agent_running: false, agent_status: "idle", context_state: "healthy" },
      }),
      { decision: "ASK_HUMAN" },
    ),
    chatCase(
      "chat_supervisor_handoff",
      supervisorOrigin,
      supervisorMessages({
        ledger: {
          objective: "Finish the long multi-stage migration.",
          todos: [{ id: "w1", title: "migrate remaining call sites", status: "open", owner: "assistant", kind: "implement" }],
        },
        authoredTurns: [{ role: "user", at: 5, text: "Continue the migration." }],
        runtime: { agent_running: false, agent_status: "idle", context_state: "rollover_required" },
        boundary: { boundary: "context_pressure", reason: "context window nearly exhausted" },
      }),
      { decision: "HANDOFF" },
    ),
    chatCase(
      "chat_judge_continue",
      judgeOrigin,
      [{
        role: "user",
        content: buildLlmJudgeUserMessage({
          userText: "帮我实现重试辅助函数并跑完整测试。",
          assistantText: "重试辅助函数已实现。下一步：我会运行完整测试套件并修复失败项。",
        }),
      }],
      { judge: "continue" },
    ),
    chatCase(
      "chat_judge_done",
      judgeOrigin,
      [{
        role: "user",
        content: buildLlmJudgeUserMessage({
          userText: "帮我实现重试辅助函数并跑完整测试。",
          assistantText: "已全部完成：辅助函数实现、测试通过、已提交，没有剩余任务。",
        }),
      }],
      { judge: "done" },
    ),
  ];
}

function buildFixture() {
  const typed = typedCases();
  const chat = chatCases();
  const ids = [...typed, ...chat].map((entry) => entry.id);
  if (new Set(ids).size !== ids.length) throw new Error("duplicate case id in generated fixture");
  return canonical({
    version: 1,
    generator: "scripts/bench/generate-semantic-cases.mjs",
    description:
      "Deterministic semantic route benchmark cases. Typed payloads come from the shipped prompt builders; "
      + "the browser fast-path cases mirror the heads in crates/herdr-mcp/src/mcp.rs browser_fast_path_decision.",
    typed_case_count: typed.length,
    chat_case_count: chat.length,
    typed_cases: typed,
    chat_cases: chat,
  });
}

const serialize = (fixture) => `${JSON.stringify(fixture, null, 2)}\n`;

function assertNoSecretLikeContent(text) {
  const forbidden = ["api_key", "apikey", "authorization", "bearer ", "sk-", "secret"];
  const lowered = text.toLowerCase();
  const hit = forbidden.find((needle) => lowered.includes(needle));
  if (hit) throw new Error(`fixture must not contain secret-like content (matched "${hit}")`);
}

const fixture = buildFixture();
const serialized = serialize(fixture);
assertNoSecretLikeContent(serialized);

const check = process.argv.includes("--check");
if (check) {
  let existing = "";
  try {
    existing = readFileSync(fixturePath, "utf8");
  } catch {
    existing = "";
  }
  if (existing !== serialized) {
    console.error(`semantic-cases: FAIL ${fixturePath} is not up to date; re-run without --check`);
    process.exit(1);
  }
  const digest = createHash("sha256").update(serialized).digest("hex").slice(0, 16);
  console.log(`semantic-cases: PASS cases=${fixture.typed_case_count}+${fixture.chat_case_count} sha256=${digest}`);
  process.exit(0);
}

mkdirSync(dirname(fixturePath), { recursive: true });
writeFileSync(fixturePath, serialized);
const digest = createHash("sha256").update(serialized).digest("hex").slice(0, 16);
console.log(
  `semantic-cases: wrote ${fixturePath} typed=${fixture.typed_case_count} chat=${fixture.chat_case_count} sha256=${digest}`,
);
