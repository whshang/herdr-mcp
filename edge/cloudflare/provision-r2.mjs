#!/usr/bin/env node
/**
 * Idempotent R2 bucket provisioner for Herdr Edge.
 *
 * Creates every `[[r2_buckets]]` bucket named in a wrangler config, treating
 * "already exists" as success, then optionally deploys the Worker. Local
 * `wrangler dev` does not need this; remote deploy does.
 */

import { spawn } from "node:child_process";
import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

export function parseR2Bindings(toml) {
  const buckets = [];
  const lines = String(toml || "").split(/\r?\n/);
  let inBlock = false;
  let current = {};
  const flush = () => {
    if (current.binding && current.bucket_name) buckets.push({ ...current });
    current = {};
  };
  for (const raw of lines) {
    const line = raw.replace(/#.*$/, "").trim();
    if (!line) continue;
    if (line === "[[r2_buckets]]") {
      flush();
      inBlock = true;
      continue;
    }
    if (line.startsWith("[") ) {
      if (inBlock) flush();
      inBlock = false;
      continue;
    }
    if (!inBlock) continue;
    const match = /^(binding|bucket_name)\s*=\s*"([^"]+)"$/.exec(line);
    if (match) current[match[1]] = match[2];
  }
  if (inBlock) flush();
  return buckets;
}

export function isAlreadyExistsError(text) {
  return /already exists/i.test(String(text || ""));
}

function parseArgs(argv) {
  const options = {
    config: "wrangler.toml",
    wrangler: "npx --yes wrangler@4",
    deploy: false,
    dryRun: false,
  };
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (arg === "--config") options.config = argv[++i];
    else if (arg === "--wrangler") options.wrangler = argv[++i];
    else if (arg === "--deploy") options.deploy = true;
    else if (arg === "--dry-run") options.dryRun = true;
    else if (arg === "--help" || arg === "-h") options.help = true;
    else throw new Error(`unknown argument: ${arg}`);
  }
  return options;
}

function runCommand(command, args, cwd) {
  return new Promise((resolve) => {
    const child = spawn(command, args, {
      cwd,
      env: process.env,
      stdio: ["ignore", "pipe", "pipe"],
      shell: false,
    });
    let stdout = "";
    let stderr = "";
    child.stdout.on("data", (chunk) => { stdout += chunk; });
    child.stderr.on("data", (chunk) => { stderr += chunk; });
    child.on("error", (error) => {
      resolve({ code: 1, stdout, stderr: String(error?.message || error) });
    });
    child.on("close", (code) => {
      resolve({ code: code ?? 1, stdout, stderr });
    });
  });
}

function splitCommand(command) {
  return String(command).trim().split(/\s+/).filter(Boolean);
}

export async function provisionR2Buckets({
  configPath,
  toml,
  wrangler = "npx --yes wrangler@4",
  deploy = false,
  dryRun = false,
  run = runCommand,
} = {}) {
  const resolved = path.resolve(configPath || "wrangler.toml");
  const source = toml ?? readFileSync(resolved, "utf8");
  const buckets = parseR2Bindings(source);
  if (buckets.length === 0) {
    throw new Error(`no [[r2_buckets]] bindings in ${resolved}`);
  }
  const cwd = path.dirname(resolved);
  const parts = splitCommand(wrangler);
  const bin = parts[0];
  const prefix = parts.slice(1);
  const created = [];
  const existing = [];
  if (dryRun) {
    return { ok: true, dryRun: true, buckets, created, existing, deployed: false };
  }
  for (const bucket of buckets) {
    const result = await run(bin, [...prefix, "r2", "bucket", "create", bucket.bucket_name], cwd);
    const output = `${result.stdout}\n${result.stderr}`;
    if (result.code === 0) {
      created.push(bucket.bucket_name);
      continue;
    }
    if (isAlreadyExistsError(output)) {
      existing.push(bucket.bucket_name);
      continue;
    }
    throw new Error(`failed to create R2 bucket ${bucket.bucket_name}: ${output.trim() || `exit ${result.code}`}`);
  }
  let deployed = false;
  if (deploy) {
    const result = await run(bin, [...prefix, "deploy", "--config", path.basename(resolved)], cwd);
    if (result.code !== 0) {
      throw new Error(`wrangler deploy failed: ${(result.stderr || result.stdout).trim()}`);
    }
    deployed = true;
  }
  return { ok: true, buckets, created, existing, deployed };
}

function printHelp() {
  process.stdout.write(`Provision Herdr Edge R2 buckets from a wrangler config.

Usage:
  node provision-r2.mjs --config wrangler.prod.toml
  node provision-r2.mjs --config wrangler.user.toml --deploy

Options:
  --config <path>     wrangler TOML (default wrangler.toml)
  --wrangler <cmd>    wrangler launcher (default: npx --yes wrangler@4)
  --deploy            deploy the Worker after buckets exist
  --dry-run           parse config only; do not call Cloudflare
`);
}

async function main() {
  const options = parseArgs(process.argv.slice(2));
  if (options.help) {
    printHelp();
    return;
  }
  const result = await provisionR2Buckets({
    configPath: options.config,
    wrangler: options.wrangler,
    deploy: options.deploy,
    dryRun: options.dryRun,
  });
  process.stdout.write(`${JSON.stringify({
    ok: true,
    buckets: result.buckets.map((bucket) => bucket.bucket_name),
    created: result.created,
    existing: result.existing,
    deployed: result.deployed,
    dryRun: Boolean(result.dryRun),
  })}\n`);
}

const invokedDirectly = process.argv[1] &&
  path.resolve(process.argv[1]) === fileURLToPath(import.meta.url);

if (invokedDirectly) {
  main().catch((error) => {
    process.stderr.write(`${error instanceof Error ? error.message : String(error)}\n`);
    process.exitCode = 1;
  });
}
