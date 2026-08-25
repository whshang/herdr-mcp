#!/usr/bin/env node
import { createHash } from "node:crypto";
import { readFile, readdir, stat, writeFile } from "node:fs/promises";
import { basename, join } from "node:path";
import { fileURLToPath } from "node:url";

export const RUST_RELEASE_TARGETS = [
  "aarch64-apple-darwin",
  "x86_64-apple-darwin",
  "aarch64-unknown-linux-gnu",
  "x86_64-unknown-linux-gnu",
  "x86_64-pc-windows-msvc",
];

export function parseCargoPackageVersion(text) {
  const packageBlock = String(text).match(/\[package\]([\s\S]*?)(?:\n\[|$)/)?.[1] || "";
  const version = packageBlock.match(/^version\s*=\s*"([^"]+)"\s*$/m)?.[1];
  if (!version) throw new Error("cannot read herdr-mcp package version");
  return version;
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

export async function buildRustReleaseManifest({ root, assetsDir, repo, tag }) {
  if (!repo || !/^[^/]+\/[^/]+$/.test(repo)) throw new Error("repo must be owner/name");
  if (!tag) throw new Error("tag is required");
  const cargo = await readFile(join(root, "crates", "herdr-mcp", "Cargo.toml"), "utf8");
  const contract = JSON.parse(await readFile(join(root, "contracts", "epoch2.json"), "utf8"));
  const version = parseCargoPackageVersion(cargo);
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
    schema_version: 1,
    product: "herdr-mcp",
    version,
    tag,
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
  if (!assetsDir || !output) throw new Error("--assets-dir and --output are required");
  const manifest = await buildRustReleaseManifest({ root, assetsDir, repo, tag });
  await writeFile(output, `${JSON.stringify(manifest, null, 2)}\n`, { mode: 0o600 });
  process.stdout.write(`${basename(output)} ${manifest.version} ${manifest.assets.length} assets\n`);
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  main().catch((error) => {
    console.error(error?.message || String(error));
    process.exitCode = 1;
  });
}
