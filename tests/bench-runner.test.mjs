import test from "node:test";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtempSync, readFileSync, readdirSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const runner = join(repoRoot, "scripts", "bench", "run-semantic-bench.mjs");

const BENCH = {
  started_at: "2026-01-01T00:00:00Z",
  runtime_version: "1.0.0",
  commit: "0".repeat(40),
  runs_per_case: 1,
  typed_pacing_ms: 600,
  chat_pacing_ms: 1200,
  attempt_timeout_ms: 15000,
  fixture_typed_cases: 2,
  fixture_chat_cases: 1,
};

function routeResult({ name, protocol, role, family, model, samples }) {
  return `${JSON.stringify({
    bench: { ...BENCH, route_filter: name },
    routes: [{ name, protocol, role, model, family, stopped_reason: null }],
    samples: samples.map((sample) => ({ route: name, protocol, role, family, ...sample })),
  }, null, 2)}\n`;
}

test("user gets honest merged benchmark evidence | Given per-route result files for two measured routes | When the runner merges them without provider calls | Then the merged metadata names every measured route and the table lists both", () => {
  const resultsDir = mkdtempSync(join(tmpdir(), "semantic-bench-merge-"));
  writeFileSync(join(resultsDir, "route-alpha.json"), routeResult({
    name: "alpha",
    protocol: "decision",
    role: "primary",
    family: "typed",
    model: "jev-test",
    samples: [
      { case_id: "typed_pending_work_continue", run: 1, ok: true, latency_ms: 300, error: null, verdict: "correct", detail: { has_unfinished_work: { expect: "true", actual: "true", match: true, noul: 0.96 } } },
      { case_id: "typed_goal_task_completed", run: 1, ok: false, latency_ms: 5000, error: "timeout", verdict: "error", detail: null },
    ],
  }));
  writeFileSync(join(resultsDir, "route-beta.json"), routeResult({
    name: "beta",
    protocol: "openai-chat",
    role: "fallback",
    family: "chat",
    model: "chat-test",
    samples: [
      { case_id: "chat_judge_done", run: 1, ok: true, latency_ms: 1500, error: null, verdict: null, expect: { judge: "done" }, content: "好的。" },
    ],
  }));

  const markdownPath = join(resultsDir, "table.md");
  const run = spawnSync(process.execPath, [runner, `--merge-only=${resultsDir}`, `--markdown=${markdownPath}`], {
    cwd: repoRoot,
    encoding: "utf8",
  });
  assert.equal(run.status, 0, run.stderr || run.stdout);

  const merged = JSON.parse(readFileSync(join(resultsDir, "samples-merged.json"), "utf8"));
  assert.deepEqual(merged.bench.route_filter, ["alpha", "beta"]);
  assert.deepEqual(merged.bench.routeFilter, ["alpha", "beta"]);
  assert.deepEqual(merged.routes.map((route) => route.name), ["alpha", "beta"]);
  assert.equal(merged.samples.length, 3);

  const table = readFileSync(markdownPath, "utf8");
  assert.equal(table.includes("| 1 | alpha | decision | primary | typed | jev-test | 2 | 1 | 1 (timeout×1) | 1/1 (100.0%) | 300 ms | 300 ms | 300 ms | 100.0% | - |"), true);
  assert.equal(table.includes("| 2 | beta | openai-chat | fallback | chat | chat-test | 1 | 1 | 0 | 1/1 (100.0%) | 1500 ms | 1500 ms | 1500 ms | 100.0% | - |"), true);
  assert.equal(table.includes("Per-question-head agreement (typed cases only):"), true);
  assert.equal(table.includes("expect true → got uncertain"), false);
});

test("user can re-render one sweep without new provider calls | Given an existing results directory | When only one route file is present | Then the merged report covers exactly that route and writes no additional route file", () => {
  const resultsDir = mkdtempSync(join(tmpdir(), "semantic-bench-merge-single-"));
  writeFileSync(join(resultsDir, "route-solo.json"), routeResult({
    name: "solo",
    protocol: "decision",
    role: "primary",
    family: "typed",
    model: "jev-test",
    samples: [
      { case_id: "typed_pending_work_done", run: 1, ok: true, latency_ms: 280, error: null, verdict: "incorrect", detail: { useful_work_remaining: { expect: "false", actual: "uncertain", match: false, noul: 0.21 } } },
    ],
  }));

  const run = spawnSync(process.execPath, [runner, `--merge-only=${resultsDir}`], { cwd: repoRoot, encoding: "utf8" });
  assert.equal(run.status, 0, run.stderr || run.stdout);

  const merged = JSON.parse(readFileSync(join(resultsDir, "samples-merged.json"), "utf8"));
  assert.deepEqual(merged.bench.route_filter, ["solo"]);
  assert.equal(merged.samples.length, 1);
  assert.equal(run.stdout.includes("| 1 | solo | decision | primary | typed |"), true);
  assert.equal(run.stdout.includes("expect false → got uncertain (noul=0.210) ×1"), true);
  assert.deepEqual(readdirSync(resultsDir).sort(), ["route-solo.json", "samples-merged.json"]);
});

/**
 * Materialise the committed latency-boundary fixture as per-route result files, so
 * the margin report can be rendered from recorded samples with no provider calls.
 */
function writeMarginBoundaryFixture(resultsDir) {
  const fixture = JSON.parse(readFileSync(join(repoRoot, "tests", "bench", "fixtures", "margin-boundary-samples.json"), "utf8"));
  for (const route of fixture.routes) {
    writeFileSync(join(resultsDir, `route-${route.name}.json`), routeResult(route));
  }
  return fixture;
}

test("user reads the margin boundary honestly | Given recorded samples just under, exactly at, and just over the typed attempt budget | When the runner renders the margin report | Then the call at the budget is flagged exhausted and the call over it shows a negative margin", () => {
  const resultsDir = mkdtempSync(join(tmpdir(), "semantic-margin-boundary-"));
  writeMarginBoundaryFixture(resultsDir);

  const run = spawnSync(process.execPath, [runner, `--merge-only=${resultsDir}`], { cwd: repoRoot, encoding: "utf8" });
  assert.equal(run.status, 0, run.stderr || run.stdout);
  const table = run.stdout;

  assert.equal(table.includes("Per-route × decision-head margin"), true);

  // Sorted sample latencies are 1799 / 1800 / 1801 / 3600 against a 1800 ms attempt
  // and 3600 ms pool budget: max sits on the pool boundary, so the attempt margin is
  // negative and three calls count as exhausting the attempt budget.
  assert.equal(table.includes("| fixture-typed-boundary | typed fast path | typed | 4 | 2 | 2 | 1800 ms | 3600 ms | 3600 ms | 1800 ms | -1800 ms | 200.0% | 3600 ms | 0 ms | yes — pool ×1 |"), true);

  // A route comfortably inside the budget keeps a positive margin and is not flagged.
  assert.equal(table.includes("| fixture-typed-comfortable | typed fast path | typed | 2 | 2 | 0 | 900 ms | 1200 ms | 1200 ms | 1800 ms | +600 ms | 66.7% | 3600 ms | +2400 ms | no |"), true);
});

test("user reads each decision head against its own budget | Given one measured typed route and one measured chat route | When the margin report renders | Then the typed head is read against 1800 ms and the chat head against 5000 ms", () => {
  const resultsDir = mkdtempSync(join(tmpdir(), "semantic-margin-heads-"));
  writeMarginBoundaryFixture(resultsDir);

  const run = spawnSync(process.execPath, [runner, `--merge-only=${resultsDir}`], { cwd: repoRoot, encoding: "utf8" });
  assert.equal(run.status, 0, run.stderr || run.stdout);
  const table = run.stdout;

  // The chat fallback head carries the 5000/12000 budget, not the typed 1800/3600 one:
  // 12000 ms is inside the chat pool allowance but over its attempt allowance.
  assert.equal(table.includes("| fixture-chat-boundary | chat fallback | chat | 3 | 2 | 1 | 5000 ms | 12000 ms | 12000 ms | 5000 ms | -7000 ms | 240.0% | 12000 ms | 0 ms | yes — pool ×1 |"), true);
  assert.equal(table.includes("| chat fallback | chat | 3 |"), true);
  assert.equal(table.includes("| typed fast path | typed | 4 |"), true);
});
