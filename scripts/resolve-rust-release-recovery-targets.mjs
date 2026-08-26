#!/usr/bin/env node
import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";

const TOKEN = /^[A-Za-z0-9._-]+$/;

function assertNonEmptyToken(value, field) {
  if (typeof value !== "string" || !TOKEN.test(value)) {
    throw new Error(`invalid Rust release ${field}`);
  }
  return value;
}

function validateMatrix(entries) {
  if (!Array.isArray(entries) || entries.length === 0) {
    throw new Error("Rust release target matrix must not be empty");
  }
  const targets = new Set();
  return entries.map((entry) => {
    if (!entry || typeof entry !== "object" || Array.isArray(entry)) {
      throw new Error("invalid Rust release target entry");
    }
    const keys = Object.keys(entry).sort();
    if (keys.length !== 2 || keys[0] !== "runner" || keys[1] !== "target") {
      throw new Error("Rust release target entry must contain only runner and target");
    }
    const runner = assertNonEmptyToken(entry.runner, "runner");
    const target = assertNonEmptyToken(entry.target, "target");
    if (targets.has(target)) throw new Error(`duplicate Rust release target: ${target}`);
    targets.add(target);
    return { runner, target };
  });
}

export function parseTargetContract(text) {
  let contract;
  try {
    contract = JSON.parse(String(text));
  } catch {
    throw new Error("invalid Rust release target contract JSON");
  }
  if (!contract || typeof contract !== "object" || Array.isArray(contract) || contract.schema_version !== 1) {
    throw new Error("unsupported Rust release target contract schema");
  }
  const keys = Object.keys(contract).sort();
  if (keys.length !== 2 || keys[0] !== "schema_version" || keys[1] !== "targets") {
    throw new Error("Rust release target contract contains unexpected fields");
  }
  return validateMatrix(contract.targets);
}

function extractBuildJob(workflowText) {
  const lines = String(workflowText).replace(/\r\n/g, "\n").split("\n");
  const starts = [];
  for (let index = 0; index < lines.length; index += 1) {
    if (/^  build:\s*$/.test(lines[index])) starts.push(index);
  }
  if (starts.length !== 1) throw new Error("legacy Rust release workflow must contain exactly one build job");
  const start = starts[0];
  let end = lines.length;
  for (let index = start + 1; index < lines.length; index += 1) {
    if (/^  [A-Za-z0-9_-]+:\s*$/.test(lines[index])) {
      end = index;
      break;
    }
  }
  return lines.slice(start + 1, end);
}

export function parseLegacyWorkflowMatrix(workflowText) {
  const lines = extractBuildJob(workflowText);
  const matrixIndexes = [];
  for (let index = 0; index < lines.length; index += 1) {
    if (/^      matrix:\s*$/.test(lines[index])) matrixIndexes.push(index);
  }
  if (matrixIndexes.length !== 1) throw new Error("legacy build job must contain exactly one static matrix");
  const matrixIndex = matrixIndexes[0];
  if (!/^        include:\s*$/.test(lines[matrixIndex + 1] || "")) {
    throw new Error("legacy build matrix must use a static include list");
  }

  const entries = [];
  let index = matrixIndex + 2;
  while (index < lines.length) {
    const line = lines[index];
    if (/^\s*$/.test(line)) {
      index += 1;
      continue;
    }
    if (!/^\s{10}/.test(line)) break;
    const runner = line.match(/^          - runner: ([A-Za-z0-9._-]+)\s*$/)?.[1];
    if (!runner) throw new Error(`unexpected legacy build matrix line: ${line.trim()}`);
    const targetLine = lines[index + 1] || "";
    const target = targetLine.match(/^            target: ([A-Za-z0-9._-]+)\s*$/)?.[1];
    if (!target) throw new Error("legacy build matrix runner must be followed by exactly one target");
    entries.push({ runner, target });
    index += 2;
  }
  return validateMatrix(entries);
}

export function parseLegacyManifestTargets(builderText) {
  const source = String(builderText).replace(/\r\n/g, "\n");
  const marker = /export const RUST_RELEASE_TARGETS\s*=\s*\[([\s\S]*?)\n\];/g;
  const matches = [...source.matchAll(marker)];
  if (matches.length !== 1) {
    throw new Error("legacy manifest builder must contain exactly one literal RUST_RELEASE_TARGETS array");
  }
  const lines = matches[0][1]
    .split("\n")
    .map((line) => line.trim())
    .filter(Boolean);
  if (lines.length === 0) throw new Error("legacy manifest target list must not be empty");
  const targets = lines.map((line, index) => {
    const match = line.match(/^"([A-Za-z0-9._-]+)"(,?)$/);
    if (!match) throw new Error(`unexpected legacy manifest target expression: ${line}`);
    if (index < lines.length - 1 && match[2] !== ",") {
      throw new Error("legacy manifest target entries must be comma-separated");
    }
    return match[1];
  });
  if (new Set(targets).size !== targets.length) throw new Error("duplicate legacy manifest target");
  return targets;
}

export function resolveLegacyRecoveryTargets(workflowText, builderText) {
  const matrix = parseLegacyWorkflowMatrix(workflowText);
  const manifestTargets = parseLegacyManifestTargets(builderText);
  const workflowTargets = matrix.map((entry) => entry.target);
  const sortedWorkflowTargets = [...workflowTargets].sort();
  const sortedManifestTargets = [...manifestTargets].sort();
  if (JSON.stringify(sortedWorkflowTargets) !== JSON.stringify(sortedManifestTargets)) {
    throw new Error("legacy workflow matrix does not match the tagged manifest target set");
  }
  return matrix;
}

export function resolveModernRecoveryTargets(contractText) {
  return {
    mode: "contract-v1",
    manifest_schema: 2,
    targets: parseTargetContract(contractText),
  };
}

export function resolveLegacyRecoverySource(workflowText, builderText) {
  return {
    mode: "legacy-source",
    manifest_schema: 1,
    targets: resolveLegacyRecoveryTargets(workflowText, builderText),
  };
}

async function main() {
  const args = process.argv.slice(2);
  const value = (name) => {
    const index = args.indexOf(name);
    return index >= 0 ? args[index + 1] : "";
  };
  const contractFile = value("--contract-file");
  const workflowFile = value("--legacy-workflow-file");
  const builderFile = value("--legacy-manifest-builder-file");
  let resolved;
  if (contractFile && !workflowFile && !builderFile) {
    resolved = resolveModernRecoveryTargets(await readFile(contractFile, "utf8"));
  } else if (!contractFile && workflowFile && builderFile) {
    resolved = resolveLegacyRecoverySource(
      await readFile(workflowFile, "utf8"),
      await readFile(builderFile, "utf8"),
    );
  } else {
    throw new Error("choose either --contract-file or both legacy tagged-source files");
  }
  process.stdout.write(`${JSON.stringify(resolved)}\n`);
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  main().catch((error) => {
    console.error(error?.message || String(error));
    process.exitCode = 1;
  });
}
