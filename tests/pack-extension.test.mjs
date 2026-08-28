import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import {
  extensionSha256Name,
  extensionZipName,
  packExtension,
  readExtensionVersion,
} from "../scripts/pack-extension.mjs";

test("pack-extension reads version from extension manifest", () => {
  assert.equal(readExtensionVersion(JSON.stringify({ version: "0.1.64" })), "0.1.64");
  assert.equal(extensionZipName("0.1.64"), "herdr-mcp-extension-0.1.64.zip");
  assert.equal(extensionSha256Name("0.1.64"), "herdr-mcp-extension-0.1.64.zip.sha256");
  assert.throws(() => readExtensionVersion("{}"), /invalid/);
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
    const result = await packExtension({ root: repoRoot, outDir });
    assert.equal(result.version, manifest.version);
    assert.equal(result.zipName, `herdr-mcp-extension-${manifest.version}.zip`);
    assert.ok(result.fileCount > 10);
  } finally {
    await rm(outDir, { recursive: true, force: true });
  }
});
