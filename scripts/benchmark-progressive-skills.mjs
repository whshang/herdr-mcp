#!/usr/bin/env node

import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import path from "node:path";

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

function parseArgs(argv) {
  let bootstrapBytes = null;
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (arg === "--bootstrap-bytes") {
      const raw = argv[index + 1];
      if (!raw || !/^\d+$/.test(raw)) {
        throw new Error("--bootstrap-bytes requires a non-negative integer");
      }
      bootstrapBytes = Number(raw);
      index += 1;
      continue;
    }
    throw new Error(`unknown argument: ${arg}`);
  }
  return { bootstrapBytes };
}

async function text(relativePath) {
  return readFile(path.join(repoRoot, relativePath), "utf8");
}

function bytes(value) {
  return Buffer.byteLength(value, "utf8");
}

function approxTokens(byteCount) {
  return Math.ceil(byteCount / 4);
}

function metric(byteCount, giantBytes) {
  return {
    bytes: byteCount,
    approx_tokens_chars4: approxTokens(byteCount),
    percent_of_giant: Number(((byteCount / giantBytes) * 100).toFixed(1)),
  };
}

const { bootstrapBytes: measuredBootstrapBytes } = parseArgs(process.argv.slice(2));
const giant = await text("assets/herdr-mcp-SKILL.md");
const globalAgents = (await text("assets/herdr/AGENTS.md")).trim();

const modulePaths = {
  "workstation-control": "assets/herdr/skills/workstation-control/SKILL.md",
  "files-search": "assets/herdr/skills/files-search/SKILL.md",
  "files-mutation": "assets/herdr/skills/files-mutation/SKILL.md",
  "git-repository": "assets/herdr/skills/git-repository/SKILL.md",
  execution: "assets/herdr/skills/execution/SKILL.md",
  "agent-dispatch": "assets/herdr/skills/agent-dispatch/SKILL.md",
  "development-orchestration": "assets/herdr/skills/development-orchestration/SKILL.md",
};

const moduleBytes = {};
for (const [id, relativePath] of Object.entries(modulePaths)) {
  moduleBytes[id] = bytes((await text(relativePath)).trim());
}

const profiles = {
  "fs-only": ["files-search"],
  "exec-only": ["execution"],
  "agent-delegation": ["workstation-control", "agent-dispatch"],
  "multi-line-development": [
    "workstation-control",
    "agent-dispatch",
    "development-orchestration",
    "git-repository",
    "files-mutation",
  ],
};

const giantBytes = bytes(giant);
const globalBytes = bytes(globalAgents);
const bootstrapBytes = measuredBootstrapBytes ?? globalBytes;
const profileMetrics = {};

for (const [name, ids] of Object.entries(profiles)) {
  const skillBytes = ids.reduce((total, id) => total + moduleBytes[id], 0);
  profileMetrics[name] = {
    skills: ids,
    skill_bytes: skillBytes,
    loaded: metric(bootstrapBytes + skillBytes, giantBytes),
    initial_skill_load_round_trips: ids.length > 0 ? 1 : 0,
    steady_state_skill_load_round_trips_per_tool_call: 0,
  };
}

const report = {
  approximation: "approx_tokens_chars4 is a byte/4 heuristic, not a tokenizer measurement",
  baseline_giant: metric(giantBytes, giantBytes),
  global_agents: metric(globalBytes, giantBytes),
  bootstrap: {
    source: measuredBootstrapBytes === null ? "global-only fallback" : "measured candidate value",
    ...metric(bootstrapBytes, giantBytes),
  },
  modules: Object.fromEntries(
    Object.entries(moduleBytes).map(([id, byteCount]) => [id, metric(byteCount, giantBytes)]),
  ),
  profiles: profileMetrics,
  round_trip_model: {
    bootstrap_calls_per_new_context: 1,
    initial_domain_load_calls: "0-1 batched call per classified task profile",
    new_capability_domain_calls: "0-1 additional batched call",
    reload_on_new_user_turn: false,
    reload_on_live_capability_refresh: false,
  },
};

process.stdout.write(`${JSON.stringify(report, null, 2)}\n`);
