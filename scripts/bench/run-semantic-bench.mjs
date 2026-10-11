#!/usr/bin/env node
// Live semantic route benchmark driver.
//
// Measures the semantic typed-decision (fast path) and chat (fallback) routes
// configured in ~/.config/herdr-mcp/config.json against the deterministic cases
// in tests/bench/fixtures/semantic-cases.json.
//
// Everything that touches a provider runs inside the Rust runtime's own test-only
// harness (crates/herdr-mcp/src/semantic_bench.rs) so the measurement uses the
// shipped providers, parsers and timeouts. This script only:
//   1. verifies the committed case fixture still matches the shipped prompt builders,
//   2. enumerates route names/protocols from config.json (read-only, never printed in full),
//   3. runs the Rust harness once per route, sequentially, with pacing,
//   4. scores the recorded samples with the shipped extension interpreters,
//   5. renders the results table.
//
// Secrets: config.json is only parsed in memory to list route names/protocols/models.
// No API key is copied, printed, or written to any artifact.
//
// Usage:
//   node scripts/bench/run-semantic-bench.mjs --results-dir=/tmp/semantic-bench
//     [--routes=a,b] [--runs=3] [--pacing-ms=600] [--chat-pacing-ms=1200]
//     [--attempt-ms=15000] [--force] [--markdown=docs/_wip/<file>.md]
//     [--score-only <results.json>]
//   Resumable: a route whose <results-dir>/route-<name>.json already exists is
//   reused unless --force is passed.

import { execFileSync, spawnSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, writeFileSync } from "node:fs";
import { homedir, tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { parseSupervisorDecision } from "../../extension/goal-supervisor-core.js";
import { interpretLlmJudgeReply } from "../../extension/binding-core.js";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "../..");
const fixturePath = resolve(repoRoot, "tests/bench/fixtures/semantic-cases.json");

// Production deadlines these numbers must be read against
// (crates/herdr-mcp/src/semantic.rs and mcp.rs).
const PRODUCTION_BUDGET_MS = Object.freeze({
  typed: { attempt: 1800, pool: 3600, source: "browser fast-path route/pool budget" },
  chat: { attempt: 5000, pool: 12000, source: "SemanticService chat attempt/pool budget" },
});

const VALUE_FLAGS = new Set(["--routes", "--runs", "--pacing-ms", "--chat-pacing-ms", "--attempt-ms", "--results-dir", "--markdown", "--score-only", "--merge-only"]);

function parseArgs(argv) {
  const options = {
    routes: null,
    runs: 3,
    pacingMs: 600,
    chatPacingMs: 1200,
    attemptMs: 15000,
    force: false,
    resultsDir: null,
    markdown: null,
    scoreOnly: null,
    mergeOnly: null,
    skipFixtureCheck: false,
  };
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    const splitAt = arg.includes("=") ? arg.indexOf("=") : -1;
    const key = splitAt >= 0 ? arg.slice(0, splitAt) : arg;
    let raw = splitAt >= 0 ? arg.slice(splitAt + 1) : null;
    if (VALUE_FLAGS.has(key) && raw === null) {
      raw = argv[index + 1] ?? "";
      index += 1;
    }
    switch (key) {
      case "--routes": options.routes = (raw || "").split(",").map((v) => v.trim()).filter(Boolean); break;
      case "--runs": options.runs = Number(raw); break;
      case "--pacing-ms": options.pacingMs = Number(raw); break;
      case "--chat-pacing-ms": options.chatPacingMs = Number(raw); break;
      case "--attempt-ms": options.attemptMs = Number(raw); break;
      case "--results-dir": options.resultsDir = raw; break;
      case "--markdown": options.markdown = raw; break;
      case "--score-only": options.scoreOnly = raw; break;
      case "--merge-only": options.mergeOnly = raw; break;
      case "--force": options.force = true; break;
      case "--skip-fixture-check": options.skipFixtureCheck = true; break;
      default: throw new Error(`unknown argument: ${arg}`);
    }
  }
  for (const key of ["runs", "pacingMs", "chatPacingMs", "attemptMs"]) {
    if (!Number.isFinite(options[key]) || options[key] <= 0) throw new Error(`${key} must be a positive number`);
  }
  return options;
}

function git(args) {
  try {
    return execFileSync("git", args, { cwd: repoRoot, encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] }).trim();
  } catch {
    return null;
  }
}

function configFilePath() {
  const override = process.env.HERDR_MCP_CONFIG_DIR?.trim();
  return override ? join(override, "config.json") : join(homedir(), ".config/herdr-mcp/config.json");
}

/**
 * Route inventory only: name, protocol, model, config order. No key is ever read out.
 */
function readRouteInventory() {
  const path = configFilePath();
  const parsed = JSON.parse(readFileSync(path, "utf8"));
  return (parsed?.semantic?.routes || []).map((route, index) => ({
    name: String(route.name || ""),
    protocol: String(route.protocol || ""),
    model: String(route.model || ""),
    order: index + 1,
    role: index === 0 ? "primary" : "fallback",
    family: String(route.protocol || "") === "openai-chat" ? "chat" : "typed",
  }));
}

function ensureFixtureMatchesBuilders() {
  const result = spawnSync("node", [resolve(here, "generate-semantic-cases.mjs"), "--check"], {
    cwd: repoRoot,
    encoding: "utf8",
  });
  const output = `${result.stdout || ""}${result.stderr || ""}`.trim();
  if (result.status !== 0) {
    throw new Error(`case fixture is stale or unreadable:\n${output}`);
  }
  return output;
}

function runRouteBench(route, options, resultsDir) {
  const out = join(resultsDir, `route-${route.name}.json`);
  if (existsSync(out) && !options.force) {
    console.log(`bench: reuse ${out}`);
    return JSON.parse(readFileSync(out, "utf8"));
  }
  const env = {
    ...process.env,
    HERDR_SEMANTIC_BENCH_CASES: fixturePath,
    HERDR_SEMANTIC_BENCH_OUT: out,
    HERDR_SEMANTIC_BENCH_ROUTES: route.name,
    HERDR_SEMANTIC_BENCH_RUNS: String(options.runs),
    HERDR_SEMANTIC_BENCH_PACING_MS: String(options.pacingMs),
    HERDR_SEMANTIC_BENCH_CHAT_PACING_MS: String(options.chatPacingMs),
    HERDR_SEMANTIC_BENCH_ATTEMPT_MS: String(options.attemptMs),
  };
  const commit = git(["rev-parse", "HEAD"]);
  if (commit) env.HERDR_SEMANTIC_BENCH_COMMIT = commit;
  console.log(`bench: route=${route.name} protocol=${route.protocol} runs=${options.runs}`);
  const result = spawnSync(
    "cargo",
    [
      "test",
      "-p",
      "herdr-mcp",
      "--bin",
      "herdr-mcp",
      "--",
      "--ignored",
      "--exact",
      "semantic::semantic_bench::live_route_bench_records_raw_samples",
      "--nocapture",
      "--test-threads=1",
    ],
    { cwd: repoRoot, env, stdio: "inherit" },
  );
  if (result.status !== 0) throw new Error(`bench harness failed for route ${route.name}`);
  if (!existsSync(out)) throw new Error(`bench harness produced no result for route ${route.name}`);
  return JSON.parse(readFileSync(out, "utf8"));
}

/**
 * Chat correctness is scored here, by the shipped interpreters, not by the harness:
 * supervisor replies must satisfy parseSupervisorDecision, judge replies the fixed
 * 好的/继续 protocol.
 */
function scoreChatSample(sample) {
  if (sample.ok !== true) return { scored: false, verdict: "error", actual: null };
  if (String(sample.case_id).startsWith("chat_supervisor_")) {
    const parsed = parseSupervisorDecision(sample.content || "");
    if (!parsed.ok) return { scored: false, verdict: "unscored", actual: `invalid:${parsed.error}` };
    return { scored: true, verdict: parsed.decision === sample.expect?.decision ? "correct" : "incorrect", actual: parsed.decision };
  }
  const judged = interpretLlmJudgeReply(sample.content || "");
  const label = judged.done ? "done" : judged.cont ? "continue" : "neither";
  if (label === "neither") return { scored: false, verdict: "unscored", actual: "neither" };
  return { scored: true, verdict: label === sample.expect?.judge ? "correct" : "incorrect", actual: label };
}

function percentile(sorted, ratio) {
  if (!sorted.length) return null;
  const rank = Math.min(sorted.length - 1, Math.max(0, Math.ceil(ratio * sorted.length) - 1));
  return sorted[rank];
}

function summarize(samples, routes) {
  const byRoute = new Map();
  for (const route of routes) byRoute.set(route.name, { route, samples: [] });
  for (const sample of samples) {
    const bucket = byRoute.get(sample.route);
    if (!bucket) continue;
    const chat = sample.family === "chat";
    const scored = chat ? scoreChatSample(sample) : {
      scored: sample.ok === true,
      verdict: sample.verdict ?? (sample.ok === true ? "unscored" : "error"),
      actual: null,
    };
    bucket.samples.push({ ...sample, ...scored });
  }
  return [...byRoute.values()].map(({ route, samples: rows }) => {
    const ok = rows.filter((row) => row.ok === true);
    const errors = rows.filter((row) => row.ok !== true);
    const errorCodes = {};
    for (const row of errors) errorCodes[row.error || "unknown"] = (errorCodes[row.error || "unknown"] || 0) + 1;
    const scored = rows.filter((row) => row.scored);
    const correct = scored.filter((row) => row.verdict === "correct");
    const latencies = ok.map((row) => row.latency_ms).sort((a, b) => a - b);
    const budget = PRODUCTION_BUDGET_MS[route.family].attempt;
    return {
      route,
      total: rows.length,
      ok: ok.length,
      errors: errors.length,
      errorCodes,
      scored: scored.length,
      correct: correct.length,
      unscored: rows.filter((row) => !row.scored && row.ok === true).length,
      accuracy: scored.length ? correct.length / scored.length : null,
      p50: percentile(latencies, 0.5),
      p95: percentile(latencies, 0.95),
      max: latencies.length ? latencies[latencies.length - 1] : null,
      withinBudget: latencies.length ? latencies.filter((value) => value <= budget).length / latencies.length : null,
      stopped: rows[0]?.stopped_reason ?? null,
      incorrect: rows.filter((row) => row.verdict === "incorrect"),
    };
  });
}

function familyAggregate(summaries, family, role) {
  const rows = summaries.filter((entry) => entry.route.family === family && (!role || entry.route.role === role));
  if (!rows.length) return null;
  const scored = rows.reduce((sum, entry) => sum + entry.scored, 0);
  const correct = rows.reduce((sum, entry) => sum + entry.correct, 0);
  const p50s = rows.map((entry) => entry.p50).filter((value) => value !== null).sort((a, b) => a - b);
  const p95s = rows.map((entry) => entry.p95).filter((value) => value !== null).sort((a, b) => a - b);
  return {
    routes: rows.length,
    accuracy: scored ? correct / scored : null,
    correct,
    scored,
    medianOfRouteP50: p50s.length ? percentile(p50s, 0.5) : null,
    worstRouteP95: p95s.length ? p95s[p95s.length - 1] : null,
  };
}

function formatAggregate(aggregate) {
  if (!aggregate) return "none";
  return `${aggregate.routes} route(s): accuracy ${aggregate.correct}/${aggregate.scored} (${formatPct(aggregate.accuracy)}), median route-p50 ${formatMs(aggregate.medianOfRouteP50)}, worst route-p95 ${formatMs(aggregate.worstRouteP95)}`;
}

function formatMs(value) {
  return value === null || value === undefined ? "-" : `${value} ms`;
}

function formatPct(value) {
  return value === null || value === undefined ? "-" : `${(value * 100).toFixed(1)}%`;
}

/**
 * Per-question-head agreement, so a route-level "incorrect" verdict can be read
 * at the granularity the controller actually consumes (one head at a time).
 */
function headBreakdown(samples) {
  const heads = new Map();
  for (const sample of samples) {
    if (sample.family !== "typed" || !sample.detail || typeof sample.detail !== "object") continue;
    for (const [head, detail] of Object.entries(sample.detail)) {
      const entry = heads.get(head) || { head, compared: 0, matches: 0, disagreements: new Map() };
      if (detail.match === true || detail.match === false) {
        entry.compared += 1;
        if (detail.match) entry.matches += 1;
        else {
          const key = `expect ${detail.expect} → got ${detail.actual}${detail.noul === undefined ? "" : ` (noul=${Number(detail.noul).toFixed(3)})`}`;
          entry.disagreements.set(key, (entry.disagreements.get(key) || 0) + 1);
        }
      }
      heads.set(head, entry);
    }
  }
  return [...heads.values()].sort((a, b) => a.head.localeCompare(b.head));
}

function renderHeadSection(samples) {
  const heads = headBreakdown(samples);
  if (!heads.length) return [];
  const lines = ["", "Per-question-head agreement (typed cases only):", "", "| head | compared | matches | agreement | disagreements |", "| --- | --- | --- | --- | --- |"];
  for (const entry of heads) {
    const disagreements = entry.disagreements.size
      ? [...entry.disagreements.entries()].map(([key, count]) => `${key} ×${count}`).join("; ")
      : "-";
    lines.push(`| ${entry.head} | ${entry.compared} | ${entry.matches} | ${formatPct(entry.compared ? entry.matches / entry.compared : null)} | ${disagreements} |`);
  }
  return lines;
}

function renderMarkdown(meta, summaries, samples = []) {
  const lines = [];
  lines.push(`### Semantic route benchmark — ${meta.startedAt}`);
  lines.push("");
  lines.push(`- commit: \`${meta.commit ?? "unknown"}\`, runtime ${meta.runtimeVersion ?? "unknown"}`);
  lines.push(`- fixture: \`tests/bench/fixtures/semantic-cases.json\` (${meta.typedCases ?? meta.fixture_typed_cases} typed + ${meta.chatCases ?? meta.fixture_chat_cases} chat cases, same cases for every route)`);
  lines.push(`- runs per case: ${meta.runs ?? meta.runs_per_case}; pacing: ${meta.pacingMs ?? meta.typed_pacing_ms} ms typed / ${meta.chatPacingMs ?? meta.chat_pacing_ms} ms chat; per-call timeout: ${meta.attemptMs ?? meta.attempt_timeout_ms} ms`);
  lines.push(`- production deadline this table is read against: typed attempt ${PRODUCTION_BUDGET_MS.typed.attempt} ms / pool ${PRODUCTION_BUDGET_MS.typed.pool} ms, chat attempt ${PRODUCTION_BUDGET_MS.chat.attempt} ms / pool ${PRODUCTION_BUDGET_MS.chat.pool} ms`);
  lines.push("");
  lines.push("| # | route | protocol | role | family | model | calls | ok | errors | accuracy | p50 | p95 | max | ≤ production attempt | stopped |");
  lines.push("| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |");
  for (const entry of summaries) {
    const errorText = entry.errors ? `${entry.errors} (${Object.entries(entry.errorCodes).map(([code, count]) => `${code}×${count}`).join(", ")})` : "0";
    lines.push([
      `| ${entry.route.order}`,
      entry.route.name,
      entry.route.protocol,
      entry.route.role,
      entry.route.family,
      entry.route.model,
      String(entry.total),
      String(entry.ok),
      errorText,
      `${entry.correct}/${entry.scored} (${formatPct(entry.accuracy)})`,
      formatMs(entry.p50),
      formatMs(entry.p95),
      formatMs(entry.max),
      formatPct(entry.withinBudget),
      entry.stopped ? `\`${entry.stopped}\`` : "-",
    ].join(" | ") + " |");
  }
  lines.push("");
  for (const [label, family] of [["typed decision (fast path)", "typed"], ["chat (fallback)", "chat"]]) {
    if (!summaries.some((entry) => entry.route.family === family)) continue;
    lines.push(`- ${label} — primary (config order 1): ${formatAggregate(familyAggregate(summaries, family, "primary"))}; fallback: ${formatAggregate(familyAggregate(summaries, family, "fallback"))}`);
  }
  lines.push(...renderHeadSection(samples));
  return `${lines.join("\n")}\n`;
}

function mergeRouteFiles(resultsDir, routeNames) {
  const samples = [];
  const routeReports = [];
  let meta = null;
  for (const name of routeNames) {
    const path = join(resultsDir, `route-${name}.json`);
    if (!existsSync(path)) continue;
    const parsed = JSON.parse(readFileSync(path, "utf8"));
    meta = meta || parsed.bench;
    for (const report of parsed.routes || []) routeReports.push(report);
    for (const sample of parsed.samples || []) samples.push(sample);
  }
  for (const report of routeReports) {
    const stopped = report.stopped_reason;
    for (const sample of samples) {
      if (sample.route === report.name && stopped) sample.stopped_reason = stopped;
    }
  }
  return { meta, routes: routeReports, samples };
}

/** Config order when the workstation has a readable config; otherwise alphabetical. */
function configRouteOrder() {
  try {
    return readRouteInventory().map((route) => route.name);
  } catch {
    return [];
  }
}

/** Route names with a recorded result file, in config order when known. */
function recordedRouteNames(resultsDir) {
  const recorded = readdirSync(resultsDir)
    .filter((name) => name.startsWith("route-") && name.endsWith(".json"))
    .map((name) => name.slice("route-".length, -".json".length));
  const order = configRouteOrder();
  return [
    ...order.filter((name) => recorded.includes(name)),
    ...recorded.filter((name) => !order.includes(name)).sort(),
  ];
}

/**
 * Metadata for a merged multi-route report. `route_filter` must name every route
 * the merged samples actually cover: inheriting the first route file's filter
 * would describe the artifact as a single-route run.
 */
function mergedMeta(baseMeta, routeNames, options, resultsDir) {
  const fixture = JSON.parse(readFileSync(fixturePath, "utf8"));
  return {
    ...(baseMeta || {}),
    startedAt: baseMeta?.started_at ?? new Date().toISOString(),
    runtimeVersion: baseMeta?.runtime_version ?? null,
    commit: baseMeta?.commit ?? git(["rev-parse", "HEAD"]),
    runs: baseMeta?.runs_per_case ?? options.runs,
    pacingMs: baseMeta?.typed_pacing_ms ?? options.pacingMs,
    chatPacingMs: baseMeta?.chat_pacing_ms ?? options.chatPacingMs,
    attemptMs: baseMeta?.attempt_timeout_ms ?? options.attemptMs,
    typedCases: fixture.typed_case_count,
    chatCases: fixture.chat_case_count,
    routeFilter: routeNames,
    route_filter: routeNames,
    resultsDir,
  };
}

/** Merge per-route results into one artifact + table, without provider calls. */
function mergeAndRender(resultsDir, routeNames, options) {
  const merged = mergeRouteFiles(resultsDir, routeNames);
  const routes = merged.routes.map((route, index) => ({
    ...route,
    order: index + 1,
    role: route.role || (index === 0 ? "primary" : "fallback"),
    family: route.family || (route.protocol === "openai-chat" ? "chat" : "typed"),
  }));
  const samples = merged.samples;
  const meta = mergedMeta(merged.meta, routeNames, options, resultsDir);
  const rawPath = join(resultsDir, "samples-merged.json");
  writeFileSync(
    rawPath,
    `${JSON.stringify({ bench: meta, routes: routes.map(({ order, ...rest }) => rest), samples }, null, 2)}\n`,
  );
  console.log(`bench: merged samples ${rawPath}`);
  return { meta, routes, samples, rawPath };
}

function main() {
  const options = parseArgs(process.argv.slice(2));
  let markdown;
  let meta;
  let summaries;

  if (options.scoreOnly) {
    const parsed = JSON.parse(readFileSync(options.scoreOnly, "utf8"));
    const routes = (parsed.routes || []).map((route, index) => ({ ...route, order: index + 1, role: route.role || (index === 0 ? "primary" : "fallback") }));
    const samples = (parsed.samples || []).map((sample) => {
      const report = (parsed.routes || []).find((entry) => entry.name === sample.route);
      return report?.stopped_reason ? { ...sample, stopped_reason: report.stopped_reason } : sample;
    });
    meta = { ...parsed.bench, startedAt: parsed.bench?.started_at ?? "unknown" };
    summaries = summarize(samples, routes);
    markdown = renderMarkdown(meta, summaries, samples);
  } else if (options.mergeOnly) {
    if (!options.skipFixtureCheck) console.log(ensureFixtureMatchesBuilders());
    const resultsDir = resolve(options.mergeOnly);
    if (!existsSync(resultsDir)) throw new Error(`results directory not found: ${resultsDir}`);
    const routeNames = recordedRouteNames(resultsDir);
    if (!routeNames.length) throw new Error(`no route-*.json results in ${resultsDir}`);
    const merged = mergeAndRender(resultsDir, routeNames, options);
    summaries = summarize(merged.samples, merged.routes);
    markdown = renderMarkdown(merged.meta, summaries, merged.samples);
  } else {
    const fixtureCheck = options.skipFixtureCheck ? "skipped" : ensureFixtureMatchesBuilders();
    if (!options.skipFixtureCheck) console.log(fixtureCheck);
    const inventory = readRouteInventory();
    const wanted = options.routes ? inventory.filter((route) => options.routes.includes(route.name)) : inventory;
    const unsupported = wanted.filter((route) => !["decision", "decision-vercel", "openai-chat"].includes(route.protocol));
    const runnable = wanted.filter((route) => !unsupported.includes(route));
    for (const route of unsupported) {
      console.log(`bench: skipping ${route.name} (unsupported protocol ${route.protocol})`);
    }
    if (!runnable.length) throw new Error("no runnable semantic routes found in config.json");
    const resultsDir = options.resultsDir || mkdtempSync(join(tmpdir(), "semantic-bench-"));
    mkdirSync(resultsDir, { recursive: true });
    console.log(`bench: results dir ${resultsDir}`);
    for (const route of runnable) runRouteBench(route, options, resultsDir);

    const merged = mergeAndRender(resultsDir, runnable.map((route) => route.name), options);
    summaries = summarize(merged.samples, merged.routes);
    markdown = renderMarkdown(merged.meta, summaries, merged.samples);
  }

  markdown = markdown || renderMarkdown(meta, summaries);
  if (options.markdown) {
    const target = resolve(repoRoot, options.markdown);
    mkdirSync(dirname(target), { recursive: true });
    writeFileSync(target, markdown);
    console.log(`bench: wrote ${target}`);
  } else {
    process.stdout.write(`\n${markdown}`);
  }
}

// Import-safe: the CLI runs only when invoked directly, so tests can import this
// module (and the pure helpers) without side effects.
const invokedDirectly = Boolean(process.argv[1]) && resolve(process.argv[1]) === fileURLToPath(import.meta.url);
if (invokedDirectly) main();
