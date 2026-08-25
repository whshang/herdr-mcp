#!/usr/bin/env node
import { createHash } from "node:crypto";
import { readFile, readdir, stat, writeFile } from "node:fs/promises";
import { basename, join } from "node:path";
import { fileURLToPath } from "node:url";

export const RUST_RELEASE_TARGETS = [
  "aarch64-apple-darwin",
  "x86_64-pc-windows-msvc",
];

export const RUST_RELEASE_PROVENANCE = Object.freeze({
  predicateType: "https://slsa.dev/provenance/v1",
  attestation: "github-artifact-attestation",
  bundleMediaType: "application/vnd.dev.sigstore.bundle.v0.3+json",
  workflow: ".github/workflows/rust-release.yml",
  workflowName: "Rust Release",
  issuer: "https://token.actions.githubusercontent.com",
  runnerEnvironment: "github-hosted",
});

export function parseCargoPackageVersion(text) {
  const packageBlock = String(text).match(/\[package\]([\s\S]*?)(?:\n\[|$)/)?.[1] || "";
  const version = packageBlock.match(/^version\s*=\s*"([^"]+)"\s*$/m)?.[1];
  if (!version) throw new Error("cannot read herdr-mcp package version");
  return version;
}

export function parseRustStateSchema(text) {
  const version = String(text).match(/pub const SCHEMA_VERSION:\s*i64\s*=\s*(\d+)\s*;/)?.[1];
  if (!version) throw new Error("cannot read Rust state schema version");
  const parsed = Number(version);
  if (!Number.isSafeInteger(parsed) || parsed <= 0) throw new Error("invalid Rust state schema version");
  return parsed;
}

export function releaseAssetName(version, target) {
  if (!RUST_RELEASE_TARGETS.includes(target)) throw new Error(`unsupported Rust release target: ${target}`);
  const suffix = target.includes("windows") ? ".exe" : "";
  return `herdr-mcp-${version}-${target}${suffix}`;
}

async function sha256File(path) {
  const bytes = await readFile(path);
  return createHash("sha256").update(bytes).digest("hex");
}

export async function buildRustReleaseManifest({
  root,
  assetsDir,
  repo,
  tag,
  repositoryId,
  sourceCommit,
  sourceRef,
  workflowName = RUST_RELEASE_PROVENANCE.workflowName,
}) {
  if (!repo || !/^[^/]+\/[^/]+$/.test(repo)) throw new Error("repo must be owner/name");
  if (!tag) throw new Error("tag is required");
  const numericRepositoryId = Number(repositoryId);
  if (!Number.isSafeInteger(numericRepositoryId) || numericRepositoryId <= 0) {
    throw new Error("repository id must be a positive integer");
  }
  if (!/^[a-f0-9]{40}$/.test(String(sourceCommit || ""))) {
    throw new Error("source commit must be a lowercase 40-character git SHA");
  }
  if (!/^refs\/(?:heads|tags)\/[A-Za-z0-9._\/-]+$/.test(String(sourceRef || ""))) {
    throw new Error("source ref must be a normalized branch or tag ref");
  }
  if (sourceRef.startsWith("refs/tags/") && sourceRef !== `refs/tags/${tag}`) {
    throw new Error("source tag ref must match release tag");
  }
  if (workflowName !== RUST_RELEASE_PROVENANCE.workflowName) {
    throw new Error(`workflow name must be ${RUST_RELEASE_PROVENANCE.workflowName}`);
  }
  const cargo = await readFile(join(root, "crates", "herdr-mcp", "Cargo.toml"), "utf8");
  const stateStore = await readFile(join(root, "crates", "herdr-mcp", "src", "state_store.rs"), "utf8");
  const contract = JSON.parse(await readFile(join(root, "contracts", "epoch2.json"), "utf8"));
  const version = parseCargoPackageVersion(cargo);
  const stateSchema = parseRustStateSchema(stateStore);
  const tagVersion = tag.startsWith("v") ? tag.slice(1) : tag;
  if (tagVersion !== version) throw new Error(`tag ${tag} does not match Cargo version ${version}`);

  const names = new Set(await readdir(assetsDir));
  const assets = [];
  for (const target of RUST_RELEASE_TARGETS) {
    const name = releaseAssetName(version, target);
    if (!names.has(name)) throw new Error(`missing Rust release asset: ${name}`);
    const path = join(assetsDir, name);
    const info = await stat(path);
    if (!info.isFile() || info.size <= 0) throw new Error(`invalid Rust release asset: ${name}`);
    assets.push({
      target,
      name,
      size: info.size,
      sha256: await sha256File(path),
      url: `https://github.com/${repo}/releases/download/${encodeURIComponent(tag)}/${encodeURIComponent(name)}`,
    });
  }

  return {
    schema_version: 2,
    product: "herdr-mcp",
    version,
    tag,
    state_schema: stateSchema,
    release_identity: {
      tag,
      source_commit: sourceCommit,
      source_ref: sourceRef,
    },
    repository_identity: {
      repository: repo,
      repository_id: numericRepositoryId,
    },
    provenance: {
      predicate_type: RUST_RELEASE_PROVENANCE.predicateType,
      attestation: RUST_RELEASE_PROVENANCE.attestation,
      bundle_media_type: RUST_RELEASE_PROVENANCE.bundleMediaType,
      workflow: RUST_RELEASE_PROVENANCE.workflow,
      workflow_name: workflowName,
      issuer: RUST_RELEASE_PROVENANCE.issuer,
      runner_environment: RUST_RELEASE_PROVENANCE.runnerEnvironment,
    },
    contract: {
      epoch: contract.contract_epoch,
      hash: contract.contract_hash,
      tool_count: contract.tool_count,
    },
    assets,
  };
}

async function main() {
  const args = process.argv.slice(2);
  const value = (name) => {
    const i = args.indexOf(name);
    return i >= 0 ? args[i + 1] : "";
  };
  const root = value("--root") || process.cwd();
  const assetsDir = value("--assets-dir");
  const output = value("--output");
  const repo = value("--repo") || process.env.GITHUB_REPOSITORY || "";
  const tag = value("--tag") || process.env.GITHUB_REF_NAME || "";
  const repositoryId = value("--repository-id") || process.env.GITHUB_REPOSITORY_ID || "";
  const sourceCommit = value("--source-commit") || process.env.GITHUB_SHA || "";
  const sourceRef = value("--source-ref") || process.env.GITHUB_REF || "";
  const workflowName = value("--workflow-name") || process.env.GITHUB_WORKFLOW || RUST_RELEASE_PROVENANCE.workflowName;
  if (!assetsDir || !output) throw new Error("--assets-dir and --output are required");
  const manifest = await buildRustReleaseManifest({
    root,
    assetsDir,
    repo,
    tag,
    repositoryId,
    sourceCommit,
    sourceRef,
    workflowName,
  });
  await writeFile(output, `${JSON.stringify(manifest, null, 2)}\n`, { mode: 0o600 });
  process.stdout.write(`${basename(output)} ${manifest.version} ${manifest.assets.length} assets\n`);
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  main().catch((error) => {
    console.error(error?.message || String(error));
    process.exitCode = 1;
  });
}
