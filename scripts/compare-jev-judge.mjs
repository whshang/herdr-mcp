#!/usr/bin/env node
import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";

import {
  DEFAULT_LLM_JUDGE_PROMPT,
  DEFAULT_LLM_SKIP_KEYWORDS_TEXT,
  buildLlmJudgeUserMessage,
  interpretLlmJudgeReply,
  llmJudgeCompletionsUrl,
  shouldAutoContinueWithoutLlm,
} from "../extension/binding-core.js";
import {
  DEFAULT_JEV_BASE_URL,
  DEFAULT_JEV_MODEL,
  DEFAULT_JEV_THRESHOLD,
  buildJevPendingWorkRequest,
  interpretJevPendingWorkAnswer,
  jevSystemOneUrl,
} from "../extension/jev-judge-core.js";

const fixturePath = fileURLToPath(new URL("../tests/fixtures/post-turn-judge-cases.json", import.meta.url));
const fixture = JSON.parse(await readFile(fixturePath, "utf8"));
const cases = Array.isArray(fixture?.cases) ? fixture.cases : [];
const validateOnly = process.argv.includes("--validate");
const jsonOutput = process.argv.includes("--json");

function requireCases() {
  if (!cases.length) throw new Error("benchmark fixture has no cases");
  const ids = new Set();
  for (const row of cases) {
    if (!row?.id || ids.has(row.id)) throw new Error(`invalid or duplicate case id: ${row?.id || ""}`);
    ids.add(row.id);
    if (!["continue", "done"].includes(row.expected)) throw new Error(`invalid expected label for ${row.id}`);
    if (!String(row.user || "").trim() || !String(row.assistant || "").trim()) {
      throw new Error(`case ${row.id} requires user and assistant text`);
    }
  }
}

function env(name, fallback = "") {
  const value = String(process.env[name] || "").trim();
  return value || fallback;
}

function llmConfig() {
  return {
    baseUrl: env("HERDR_LLM_JUDGE_BASE_URL"),
    apiKey: env("HERDR_LLM_JUDGE_API_KEY"),
    model: env("HERDR_LLM_JUDGE_MODEL"),
    prompt: DEFAULT_LLM_JUDGE_PROMPT,
    skipKeywords: DEFAULT_LLM_SKIP_KEYWORDS_TEXT,
  };
}

function jevConfig() {
  return {
    baseUrl: env("TYPESAFE_BASE_URL", DEFAULT_JEV_BASE_URL),
    apiKey: env("TYPESAFE_API_KEY"),
    model: env("TYPESAFE_MODEL", DEFAULT_JEV_MODEL),
    threshold: DEFAULT_JEV_THRESHOLD,
  };
}

async function postJson(url, apiKey, body, timeoutMs) {
  const started = performance.now();
  try {
    const response = await fetch(url, {
      method: "POST",
      headers: {
        "content-type": "application/json",
        authorization: `Bearer ${apiKey}`,
      },
      body: JSON.stringify(body),
      signal: AbortSignal.timeout(timeoutMs),
    });
    const elapsedMs = performance.now() - started;
    if (!response.ok) {
      return {
        ok: false,
        latency_ms: elapsedMs,
        reason: "http",
        status: response.status,
        error: (await response.text().catch(() => "")).slice(0, 200),
      };
    }
    return { ok: true, latency_ms: elapsedMs, body: await response.json() };
  } catch (error) {
    return {
      ok: false,
      latency_ms: performance.now() - started,
      reason: error?.name === "TimeoutError" || error?.name === "AbortError" ? "timeout" : "network",
      error: error?.message || String(error),
    };
  }
}

async function runLlm(row, cfg) {
  const prompt = buildLlmJudgeUserMessage(cfg.prompt, {
    userText: row.user,
    assistantText: row.assistant,
  });
  const response = await postJson(
    llmJudgeCompletionsUrl(cfg.baseUrl),
    cfg.apiKey,
    {
      model: cfg.model,
      messages: [{ role: "user", content: prompt }],
      temperature: 0,
      stream: false,
    },
    60_000,
  );
  if (!response.ok) return response;
  const content = response.body?.choices?.[0]?.message?.content;
  if (typeof content !== "string") return { ...response, ok: false, reason: "bad_response" };
  const verdict = interpretLlmJudgeReply(content, { skipKeywords: cfg.skipKeywords });
  return {
    ok: true,
    latency_ms: response.latency_ms,
    verdict,
    prediction: verdict.cont ? "continue" : verdict.done ? "done" : "uncertain",
  };
}

async function runJev(row, cfg) {
  const response = await postJson(
    jevSystemOneUrl(cfg.baseUrl),
    cfg.apiKey,
    buildJevPendingWorkRequest(row.user, row.assistant, cfg.model),
    15_000,
  );
  if (!response.ok) return response;
  const verdict = interpretJevPendingWorkAnswer(response.body, cfg.threshold);
  return {
    ...verdict,
    latency_ms: response.latency_ms,
    prediction: verdict.ok ? verdict.signal : "uncertain",
  };
}

function combinePriority(row, llm, jev) {
  if (jev?.ok && jev.prediction === "continue") return { ok: true, prediction: "continue", source: "jev" };
  if (jev?.ok && jev.prediction === "done") return { ok: true, prediction: "done", source: "jev" };
  if (llm?.ok && (llm.prediction === "continue" || llm.prediction === "done")) {
    return { ok: true, prediction: llm.prediction, source: "llm" };
  }
  return {
    ok: true,
    prediction: shouldAutoContinueWithoutLlm(row.user, row.assistant) ? "continue" : "done",
    source: "script",
  };
}

function percentile(values, p) {
  if (!values.length) return null;
  const sorted = [...values].sort((a, b) => a - b);
  const index = Math.min(sorted.length - 1, Math.max(0, Math.ceil(sorted.length * p) - 1));
  return sorted[index];
}

function summarize(name, results) {
  const decided = results.filter((r) => r.prediction === "continue" || r.prediction === "done");
  const correct = results.filter((r) => r.prediction === r.expected).length;
  const falseDone = results.filter((r) => r.expected === "continue" && r.prediction === "done").length;
  const falseContinue = results.filter((r) => r.expected === "done" && r.prediction === "continue").length;
  const uncertain = results.filter((r) => r.prediction === "uncertain" || r.prediction === "unavailable").length;
  const decidedCorrect = decided.filter((r) => r.prediction === r.expected).length;
  const latencies = results.map((r) => r.latency_ms).filter(Number.isFinite);
  return {
    name,
    cases: results.length,
    correct,
    accuracy: results.length ? correct / results.length : 0,
    coverage: results.length ? decided.length / results.length : 0,
    decided_accuracy: decided.length ? decidedCorrect / decided.length : 0,
    false_done: falseDone,
    false_continue: falseContinue,
    uncertain,
    latency_ms: latencies.length ? {
      mean: latencies.reduce((a, b) => a + b, 0) / latencies.length,
      p50: percentile(latencies, 0.5),
      p95: percentile(latencies, 0.95),
    } : null,
  };
}

function printSummary(summary) {
  const pct = (v) => `${(v * 100).toFixed(1)}%`;
  const latency = summary.latency_ms
    ? ` mean=${summary.latency_ms.mean.toFixed(0)}ms p50=${summary.latency_ms.p50.toFixed(0)}ms p95=${summary.latency_ms.p95.toFixed(0)}ms`
    : "";
  console.log(
    `${summary.name.padEnd(8)} accuracy=${pct(summary.accuracy)} coverage=${pct(summary.coverage)}`
    + ` decided_accuracy=${pct(summary.decided_accuracy)} false_done=${summary.false_done}`
    + ` false_continue=${summary.false_continue} uncertain=${summary.uncertain}${latency}`,
  );
}

requireCases();
if (validateOnly) {
  console.log(`post-turn judge benchmark fixture: PASS (${cases.length} cases)`);
  process.exit(0);
}

const llm = llmConfig();
const jev = jevConfig();
const missing = [];
if (!llm.baseUrl) missing.push("HERDR_LLM_JUDGE_BASE_URL");
if (!llm.apiKey) missing.push("HERDR_LLM_JUDGE_API_KEY");
if (!llm.model) missing.push("HERDR_LLM_JUDGE_MODEL");
if (!jev.apiKey) missing.push("TYPESAFE_API_KEY");
if (missing.length) {
  console.error(`live benchmark skipped: missing ${missing.join(", ")}`);
  console.error("Run with --validate to check the fixture without network calls.");
  process.exit(2);
}

const rows = [];
for (const row of cases) {
  const [llmResult, jevResult] = await Promise.all([runLlm(row, llm), runJev(row, jev)]);
  const auto = combinePriority(row, llmResult, jevResult);
  rows.push({
    id: row.id,
    expected: row.expected,
    llm: llmResult,
    jev: jevResult,
    auto,
  });
}

const llmResults = rows.map((r) => ({ ...r.llm, expected: r.expected }));
const jevResults = rows.map((r) => ({ ...r.jev, expected: r.expected }));
const autoResults = rows.map((r) => ({
  ...r.auto,
  expected: r.expected,
  latency_ms: Math.max(Number(r.llm.latency_ms) || 0, Number(r.jev.latency_ms) || 0),
}));
const summaries = [
  summarize("LLM", llmResults),
  summarize("Jev", jevResults),
  summarize("Auto", autoResults),
];

if (jsonOutput) {
  console.log(JSON.stringify({ schema_version: 1, summaries, rows }, null, 2));
} else {
  for (const summary of summaries) printSummary(summary);
  console.log("");
  for (const row of rows) {
    const jevP = row.jev?.ok ? Number(row.jev.probability).toFixed(3) : row.jev?.reason || "error";
    console.log(
      `${row.id}: expected=${row.expected} llm=${row.llm.prediction || row.llm.reason}`
      + ` jev=${row.jev.prediction || row.jev.reason}(${jevP}) auto=${row.auto.prediction}(${row.auto.source})`,
    );
  }
}
