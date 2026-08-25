import assert from "node:assert/strict";
import { mkdtemp, mkdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import {
  RUST_RELEASE_PROVENANCE,
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
      repositoryId: "1340180695",
      sourceCommit: "a".repeat(40),
      sourceRef: "refs/tags/v1.2.3-alpha.4",
    });
    assert.equal(manifest.schema_version, 2);
    assert.equal(manifest.version, "1.2.3-alpha.4");
    assert.equal(manifest.state_schema, 4);
    assert.deepEqual(manifest.release_identity, {
      tag: "v1.2.3-alpha.4",
      source_commit: "a".repeat(40),
      source_ref: "refs/tags/v1.2.3-alpha.4",
    });
    assert.deepEqual(manifest.repository_identity, {
      repository: "whshang/herdr-mcp",
      repository_id: 1340180695,
    });
    assert.deepEqual(manifest.provenance, {
      predicate_type: RUST_RELEASE_PROVENANCE.predicateType,
      attestation: RUST_RELEASE_PROVENANCE.attestation,
      bundle_media_type: RUST_RELEASE_PROVENANCE.bundleMediaType,
      workflow: RUST_RELEASE_PROVENANCE.workflow,
      workflow_name: RUST_RELEASE_PROVENANCE.workflowName,
      issuer: RUST_RELEASE_PROVENANCE.issuer,
      runner_environment: RUST_RELEASE_PROVENANCE.runnerEnvironment,
    });
    assert.deepEqual(manifest.contract, { epoch: 2, hash: "sha256:test", tool_count: 18 });
    assert.deepEqual(RUST_RELEASE_TARGETS, [
      "aarch64-apple-darwin",
      "x86_64-pc-windows-msvc",
    ]);
    assert.equal(manifest.assets.length, 2);
    assert.deepEqual(manifest.assets.map((asset) => asset.target), RUST_RELEASE_TARGETS);
    assert.ok(manifest.assets.every((asset) => /^[a-f0-9]{64}$/.test(asset.sha256)));
    assert.ok(manifest.assets.every((asset) => asset.size > 0));
    assert.ok(manifest.assets.every((asset) => asset.url.includes("/releases/download/v1.2.3-alpha.4/")));

    const rehearsal = await buildRustReleaseManifest({
      root,
      assetsDir,
      repo: "whshang/herdr-mcp",
      tag: "v1.2.3-alpha.4",
      repositoryId: "1340180695",
      sourceCommit: "b".repeat(40),
      sourceRef: "refs/heads/main",
    });
    assert.equal(rehearsal.release_identity.source_ref, "refs/heads/main");
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
      buildRustReleaseManifest({
        root,
        assetsDir,
        repo: "o/r",
        tag: "v2.0.0",
        repositoryId: "123",
        sourceCommit: "a".repeat(40),
        sourceRef: "refs/tags/v2.0.0",
      }),
      /does not match Cargo version/,
    );
    await assert.rejects(
      buildRustReleaseManifest({
        root,
        assetsDir,
        repo: "o/r",
        tag: "v1.0.0",
        repositoryId: "123",
        sourceCommit: "a".repeat(40),
        sourceRef: "refs/tags/v1.0.0",
      }),
      /missing Rust release asset/,
    );
    await assert.rejects(
      buildRustReleaseManifest({
        root,
        assetsDir,
        repo: "o/r",
        tag: "v1.0.0",
        repositoryId: "123",
        sourceCommit: "not-a-sha",
        sourceRef: "refs/tags/v1.0.0",
      }),
      /source commit/,
    );
    await assert.rejects(
      buildRustReleaseManifest({
        root,
        assetsDir,
        repo: "o/r",
        tag: "v1.0.0",
        repositoryId: "123",
        sourceCommit: "a".repeat(40),
        sourceRef: "refs/tags/v9.9.9",
      }),
      /source tag ref/,
    );
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});
