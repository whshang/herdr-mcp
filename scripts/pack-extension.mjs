#!/usr/bin/env node
/**
 * Build a deterministic zip of extension/ for GitHub Release distribution.
 *
 * Output: herdr-mcp-extension-<manifest.version>.zip (+ .sha256 sidecar)
 * Zip root contains the unpacked extension files (manifest.json at top level)
 * so: unzip -d ~/.config/herdr-mcp/extension herdr-mcp-extension-<ver>.zip
 *
 * Not listed in release-manifest.json (updater schema stays platform binaries only).
 */
import { createHash } from "node:crypto";
import { mkdir, readdir, readFile, stat, writeFile } from "node:fs/promises";
import { join, relative, sep } from "node:path";
import { fileURLToPath } from "node:url";

const SKIP_NAMES = new Set([
  ".DS_Store",
  ".git",
  ".gitignore",
  "Thumbs.db",
  "__MACOSX",
]);

const DOS_EPOCH = { time: 0, date: 0x21 }; // 1980-01-01 00:00:00

export function readExtensionVersion(manifestText) {
  const manifest = JSON.parse(manifestText);
  const version = String(manifest?.version || "").trim();
  if (!/^\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.-]+)?$/.test(version)) {
    throw new Error(`extension manifest version is invalid: ${version || "(empty)"}`);
  }
  return version;
}

export function extensionZipName(version) {
  return `herdr-mcp-extension-${version}.zip`;
}

export function extensionSha256Name(version) {
  return `${extensionZipName(version)}.sha256`;
}

function shouldSkip(name) {
  return SKIP_NAMES.has(name) || name.startsWith(".");
}

async function listExtensionFiles(extensionDir) {
  const files = [];
  async function walk(dir) {
    const entries = await readdir(dir, { withFileTypes: true });
    entries.sort((a, b) => a.name.localeCompare(b.name));
    for (const entry of entries) {
      if (shouldSkip(entry.name)) continue;
      const full = join(dir, entry.name);
      if (entry.isDirectory()) {
        await walk(full);
        continue;
      }
      if (!entry.isFile()) continue;
      const rel = relative(extensionDir, full).split(sep).join("/");
      files.push({ abs: full, rel });
    }
  }
  await walk(extensionDir);
  files.sort((a, b) => a.rel.localeCompare(b.rel));
  return files;
}

function crc32(buf) {
  let crc = 0xffffffff;
  for (let i = 0; i < buf.length; i += 1) {
    crc ^= buf[i];
    for (let j = 0; j < 8; j += 1) {
      const bit = crc & 1;
      crc = (crc >>> 1) ^ (bit ? 0xedb88320 : 0);
    }
  }
  return (crc ^ 0xffffffff) >>> 0;
}

function u16(n) {
  const b = Buffer.alloc(2);
  b.writeUInt16LE(n, 0);
  return b;
}

function u32(n) {
  const b = Buffer.alloc(4);
  b.writeUInt32LE(n >>> 0, 0);
  return b;
}

/** STORED zip with fixed DOS timestamps and sorted paths (byte-identical for identical input). */
export async function buildDeterministicZip(files) {
  const localChunks = [];
  const centralChunks = [];
  let offset = 0;

  for (const file of files) {
    const data = await readFile(file.abs);
    const nameBuf = Buffer.from(file.rel, "utf8");
    const crc = crc32(data);
    const size = data.length;
    const localHeader = Buffer.concat([
      u32(0x04034b50),
      u16(20),
      u16(0),
      u16(0),
      u16(DOS_EPOCH.time),
      u16(DOS_EPOCH.date),
      u32(crc),
      u32(size),
      u32(size),
      u16(nameBuf.length),
      u16(0),
      nameBuf,
    ]);
    localChunks.push(localHeader, data);

    const central = Buffer.concat([
      u32(0x02014b50),
      u16(20),
      u16(20),
      u16(0),
      u16(0),
      u16(DOS_EPOCH.time),
      u16(DOS_EPOCH.date),
      u32(crc),
      u32(size),
      u32(size),
      u16(nameBuf.length),
      u16(0),
      u16(0),
      u16(0),
      u16(0),
      u32(0),
      u32(offset),
      nameBuf,
    ]);
    centralChunks.push(central);
    offset += localHeader.length + data.length;
  }

  const centralDir = Buffer.concat(centralChunks);
  const end = Buffer.concat([
    u32(0x06054b50),
    u16(0),
    u16(0),
    u16(files.length),
    u16(files.length),
    u32(centralDir.length),
    u32(offset),
    u16(0),
  ]);
  return Buffer.concat([...localChunks, centralDir, end]);
}

export async function packExtension({
  root,
  outDir,
  extensionDir,
  writeSidecar = true,
} = {}) {
  const resolvedRoot = root || process.cwd();
  const src = extensionDir || join(resolvedRoot, "extension");
  const dest = outDir || join(resolvedRoot, "release-assets");
  const manifestPath = join(src, "manifest.json");
  const info = await stat(src).catch(() => null);
  if (!info?.isDirectory()) {
    throw new Error(`extension directory missing: ${src}`);
  }
  const version = readExtensionVersion(await readFile(manifestPath, "utf8"));
  const files = await listExtensionFiles(src);
  if (files.length === 0) {
    throw new Error("extension directory has no packable files");
  }
  if (!files.some((f) => f.rel === "manifest.json")) {
    throw new Error("extension pack must include manifest.json at zip root");
  }

  const zipBytes = await buildDeterministicZip(files);
  const sha256 = createHash("sha256").update(zipBytes).digest("hex");
  const zipName = extensionZipName(version);
  const shaName = extensionSha256Name(version);
  await mkdir(dest, { recursive: true });
  const zipPath = join(dest, zipName);
  await writeFile(zipPath, zipBytes, { mode: 0o644 });
  if (writeSidecar) {
    const sidecar = `${sha256}  ${zipName}\n`;
    await writeFile(join(dest, shaName), sidecar, { mode: 0o644 });
  }
  return {
    version,
    zipName,
    sha256Name: shaName,
    zipPath,
    sha256,
    fileCount: files.length,
    size: zipBytes.length,
  };
}

async function main() {
  const args = process.argv.slice(2);
  const value = (name) => {
    const i = args.indexOf(name);
    return i >= 0 ? args[i + 1] : "";
  };
  const root = value("--root") || process.cwd();
  const outDir = value("--out-dir") || join(root, "release-assets");
  const writeSidecar = !args.includes("--no-sidecar");
  const result = await packExtension({ root, outDir, writeSidecar });
  process.stdout.write(
    `${result.zipName} version=${result.version} files=${result.fileCount} size=${result.size} sha256=${result.sha256}\n`,
  );
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  main().catch((error) => {
    console.error(error?.message || String(error));
    process.exitCode = 1;
  });
}
