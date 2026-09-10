import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import {
  extensionSha256Name,
  extensionZipName,
  injectManifestKey,
  packageZipName,
  packExtension,
  readExtensionVersion,
  standaloneExtensionZipName,
} from "../scripts/pack-extension.mjs";

function readStoredZipEntry(zipBytes, wantedName) {
  let offset = 0;
  while (offset + 30 <= zipBytes.length && zipBytes.readUInt32LE(offset) === 0x04034b50) {
    const size = zipBytes.readUInt32LE(offset + 18);
    const nameLength = zipBytes.readUInt16LE(offset + 26);
    const extraLength = zipBytes.readUInt16LE(offset + 28);
    const nameStart = offset + 30;
    const dataStart = nameStart + nameLength + extraLength;
    const name = zipBytes.subarray(nameStart, nameStart + nameLength).toString("utf8");
    if (name === wantedName) return zipBytes.subarray(dataStart, dataStart + size);
    offset = dataStart + size;
  }
  throw new Error(`zip entry not found: ${wantedName}`);
}

test("pack-extension reads version from extension manifest", () => {
  assert.equal(readExtensionVersion(JSON.stringify({ version: "0.1.64" })), "0.1.64");
  assert.equal(extensionZipName("0.1.64"), "herdr-mcp-extension-0.1.64.zip");
  assert.equal(standaloneExtensionZipName("0.1.64"), "herdr-mcp-extension-standalone-0.1.64.zip");
  assert.equal(packageZipName("0.1.64", "standalone"), "herdr-mcp-extension-standalone-0.1.64.zip");
  assert.equal(extensionSha256Name("0.1.64"), "herdr-mcp-extension-0.1.64.zip.sha256");
  assert.equal(extensionSha256Name("0.1.64", "standalone"), "herdr-mcp-extension-standalone-0.1.64.zip.sha256");
  assert.throws(() => readExtensionVersion("{}"), /invalid/);
});

test("standalone manifest key injection preserves source and rejects conflicts", () => {
  const source = '{\n  "manifest_version": 3,\n  "name": "Herdr",\n  "version": "0.1.92"\n}\n';
  const injected = injectManifestKey(source, "public-key");
  assert.equal(JSON.parse(injected).key, "public-key");
  assert.equal(source.includes('"key"'), false);
  assert.throws(
    () => injectManifestKey('{"manifest_version":3,"version":"0.1.92","key":"other"}\n', "public-key"),
    /conflicting key/,
  );
});

test("pack-extension produces versioned zip with stable checksum for identical input", async () => {
  const root = await mkdtemp(join(tmpdir(), "herdr-pack-ext-"));
  const extensionDir = join(root, "extension");
  const outDir = join(root, "out");
  await mkdir(extensionDir, { recursive: true });
  await mkdir(join(extensionDir, "icons"), { recursive: true });
  await writeFile(
    join(extensionDir, "manifest.json"),
    `${JSON.stringify({ manifest_version: 3, name: "t", version: "9.8.7" }, null, 2)}\n`,
  );
  await writeFile(join(extensionDir, "background.js"), "console.log('pack');\n");
  await writeFile(join(extensionDir, "icons", "icon16.png"), Buffer.from([1, 2, 3, 4]));
  await writeFile(join(extensionDir, ".DS_Store"), "junk");
  try {
    const first = await packExtension({ root, extensionDir, outDir });
    assert.equal(first.version, "9.8.7");
    assert.equal(first.zipName, "herdr-mcp-extension-9.8.7.zip");
    assert.match(first.sha256, /^[a-f0-9]{64}$/);
    const zipBytes = await readFile(first.zipPath);
    assert.equal(createHash("sha256").update(zipBytes).digest("hex"), first.sha256);
    const sidecar = await readFile(join(outDir, first.sha256Name), "utf8");
    assert.equal(sidecar, `${first.sha256}  ${first.zipName}\n`);

    const second = await packExtension({ root, extensionDir, outDir });
    assert.equal(second.sha256, first.sha256);
    assert.deepEqual(await readFile(second.zipPath), zipBytes);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test("repo pack-extension uses live extension manifest version", async () => {
  const repoRoot = new URL("../", import.meta.url).pathname;
  const outDir = await mkdtemp(join(tmpdir(), "herdr-pack-ext-live-"));
  try {
    const manifest = JSON.parse(await readFile(join(repoRoot, "extension", "manifest.json"), "utf8"));
    assert.ok(
      typeof manifest.description === "string" && manifest.description.length <= 132,
      `Chrome Web Store manifest description must be <= 132 characters; got ${manifest.description?.length ?? "missing"}`,
    );
    const expectedIcons = { 16: "icons/icon16.png", 32: "icons/icon32.png", 48: "icons/icon48.png", 128: "icons/icon128.png" };
    assert.deepEqual(manifest.icons, expectedIcons);
    assert.deepEqual(manifest.action?.default_icon, expectedIcons);
    for (const [sizeText, relativePath] of Object.entries(expectedIcons)) {
      const size = Number(sizeText);
      const bytes = await readFile(join(repoRoot, "extension", relativePath));
      assert.deepEqual([...bytes.subarray(0, 8)], [137, 80, 78, 71, 13, 10, 26, 10], `${relativePath} must be PNG`);
      assert.equal(bytes.readUInt32BE(16), size, `${relativePath} width`);
      assert.equal(bytes.readUInt32BE(20), size, `${relativePath} height`);
    }
    const result = await packExtension({ root: repoRoot, outDir });
    assert.equal(result.version, manifest.version);
    assert.equal(result.zipName, `herdr-mcp-extension-${manifest.version}.zip`);
    assert.ok(result.fileCount > 10);
  } finally {
    await rm(outDir, { recursive: true, force: true });
  }
});

test("repo standalone package injects the fixed contract key without mutating source", async () => {
  const repoRoot = new URL("../", import.meta.url).pathname;
  const outDir = await mkdtemp(join(tmpdir(), "herdr-pack-ext-standalone-"));
  const manifestPath = join(repoRoot, "extension", "manifest.json");
  try {
    const source = await readFile(manifestPath, "utf8");
    const sourceManifest = JSON.parse(source);
    const contract = JSON.parse(
      await readFile(join(repoRoot, "contracts", "browser-extension-standalone.json"), "utf8"),
    );
    const result = await packExtension({ root: repoRoot, outDir, channel: "standalone" });
    assert.equal(result.channel, "standalone");
    assert.equal(result.zipName, `herdr-mcp-extension-standalone-${sourceManifest.version}.zip`);
    assert.match(result.sha256, /^[a-f0-9]{64}$/);

    const zipBytes = await readFile(result.zipPath);
    const packagedManifest = JSON.parse(readStoredZipEntry(zipBytes, "manifest.json").toString("utf8"));
    assert.equal(packagedManifest.key, contract.standalone.manifest_key);
    assert.equal(sourceManifest.key, undefined);
    assert.equal(await readFile(manifestPath, "utf8"), source);

    const second = await packExtension({ root: repoRoot, outDir, channel: "standalone" });
    assert.equal(second.sha256, result.sha256);
  } finally {
    await rm(outDir, { recursive: true, force: true });
  }
});
