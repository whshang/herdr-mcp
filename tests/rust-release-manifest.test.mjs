import assert from "node:assert/strict";
import { mkdtemp, mkdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import {
  RUST_RELEASE_TARGETS,
  buildRustReleaseManifest,
  parseCargoPackageVersion,
  parseRustStateSchema,
  releaseAssetName,
} from "../scripts/build-rust-release-manifest.mjs";

test("Rust release manifest is target-complete, hashed, and contract-pinned", async () => {
  const root = await mkdtemp(join(tmpdir(), "herdr-rust-release-"));
  const assetsDir = join(root, "assets");
  await mkdir(join(root, "crates", "herdr-mcp", "src"), { recursive: true });
  await mkdir(join(root, "contracts"), { recursive: true });
  await mkdir(assetsDir, { recursive: true });
  await writeFile(join(root, "crates", "herdr-mcp", "Cargo.toml"), '[package]\nname = "herdr-mcp"\nversion = "1.2.3-alpha.4"\n');
  await writeFile(join(root, "crates", "herdr-mcp", "src", "state_store.rs"), 'pub const SCHEMA_VERSION: i64 = 4;\n');
  await writeFile(join(root, "contracts", "epoch2.json"), JSON.stringify({
    contract_epoch: 2,
    contract_hash: "sha256:test",
    tool_count: 18,
  }));
  for (const target of RUST_RELEASE_TARGETS) {
    await writeFile(join(assetsDir, releaseAssetName("1.2.3-alpha.4", target)), `binary-${target}`);
  }
  try {
    const manifest = await buildRustReleaseManifest({
      root,
      assetsDir,
      repo: "whshang/herdr-mcp",
      tag: "v1.2.3-alpha.4",
    });
    assert.equal(manifest.version, "1.2.3-alpha.4");
    assert.equal(manifest.state_schema, 4);
    assert.deepEqual(manifest.contract, { epoch: 2, hash: "sha256:test", tool_count: 18 });
    assert.equal(manifest.assets.length, 5);
    assert.ok(manifest.assets.every((asset) => /^[a-f0-9]{64}$/.test(asset.sha256)));
    assert.ok(manifest.assets.every((asset) => asset.size > 0));
    assert.ok(manifest.assets.every((asset) => asset.url.includes("/releases/download/v1.2.3-alpha.4/")));
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test("Rust release manifest refuses incomplete assets and tag/version drift", async () => {
  assert.equal(parseCargoPackageVersion('[package]\nversion = "0.4.0-alpha.1"\n'), "0.4.0-alpha.1");
  assert.equal(parseRustStateSchema('pub const SCHEMA_VERSION: i64 = 4;'), 4);
  assert.throws(() => releaseAssetName("1.0.0", "mips-unknown-linux-gnu"), /unsupported/);
  const root = await mkdtemp(join(tmpdir(), "herdr-rust-release-missing-"));
  const assetsDir = join(root, "assets");
  await mkdir(join(root, "crates", "herdr-mcp", "src"), { recursive: true });
  await mkdir(join(root, "contracts"), { recursive: true });
  await mkdir(assetsDir, { recursive: true });
  await writeFile(join(root, "crates", "herdr-mcp", "Cargo.toml"), '[package]\nversion = "1.0.0"\n');
  await writeFile(join(root, "crates", "herdr-mcp", "src", "state_store.rs"), 'pub const SCHEMA_VERSION: i64 = 4;\n');
  await writeFile(join(root, "contracts", "epoch2.json"), JSON.stringify({ contract_epoch: 2, contract_hash: "h", tool_count: 18 }));
  try {
    await assert.rejects(
      buildRustReleaseManifest({ root, assetsDir, repo: "o/r", tag: "v2.0.0" }),
      /does not match Cargo version/,
    );
    await assert.rejects(
      buildRustReleaseManifest({ root, assetsDir, repo: "o/r", tag: "v1.0.0" }),
      /missing Rust release asset/,
    );
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});
