import assert from "node:assert/strict";
import { mkdtemp, mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import {
  RUST_RELEASE_MATRIX,
  RUST_RELEASE_PROVENANCE,
  RUST_RELEASE_TARGETS,
  buildRustReleaseManifest,
  edgeReleaseAssetName,
  parseCargoPackageVersion,
  parseRustStateSchema,
  releaseAssetName,
} from "../scripts/build-rust-release-manifest.mjs";

test("user receives a complete trusted manifest | Given all release assets | When building the manifest | Then assets and identity are pinned", async () => {
  const root = await mkdtemp(join(tmpdir(), "herdr-rust-release-"));
  const assetsDir = join(root, "assets");
  await mkdir(join(root, "crates", "herdr-mcp", "src"), { recursive: true });
  await mkdir(join(root, "contracts"), { recursive: true });
  await mkdir(assetsDir, { recursive: true });
  await writeFile(join(root, "crates", "herdr-mcp", "Cargo.toml"), '[package]\nname = "herdr-mcp"\nversion = "1.2.3-alpha.4"\n');
  await writeFile(join(root, "crates", "herdr-mcp", "src", "state_store.rs"), 'pub const SCHEMA_VERSION: i64 = 4;\n');
  await writeFile(join(root, "contracts", "runtime-exec-v5.json"), JSON.stringify({
    contract_epoch: 3,
    contract_hash: "sha256:test",
    tool_count: 18,
  }));
  for (const target of RUST_RELEASE_TARGETS) {
    await writeFile(join(assetsDir, releaseAssetName("1.2.3-alpha.4", target)), `binary-${target}`);
  }
  await writeFile(join(assetsDir, edgeReleaseAssetName("1.2.3-alpha.4")), "export default { fetch() {} };\n");
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
    assert.deepEqual(manifest.contract, { epoch: 3, hash: "sha256:test", tool_count: 18 });
    assert.equal(Object.hasOwn(manifest, "contract_migration"), false);
    assert.deepEqual(RUST_RELEASE_MATRIX, [
      { runner: "macos-15", target: "aarch64-apple-darwin" },
      { runner: "ubuntu-24.04", target: "x86_64-unknown-linux-musl" },
      { runner: "ubuntu-24.04-arm", target: "aarch64-unknown-linux-musl" },
      { runner: "windows-2025", target: "x86_64-pc-windows-msvc" },
      { runner: "windows-11-arm", target: "aarch64-pc-windows-msvc" },
    ]);
    assert.deepEqual(RUST_RELEASE_TARGETS, [
      "aarch64-apple-darwin",
      "x86_64-unknown-linux-musl",
      "aarch64-unknown-linux-musl",
      "x86_64-pc-windows-msvc",
      "aarch64-pc-windows-msvc",
    ]);
    assert.equal(manifest.assets.length, 5);
    assert.deepEqual(manifest.assets.map((asset) => asset.target), RUST_RELEASE_TARGETS);
    assert.ok(manifest.assets.every((asset) => /^[a-f0-9]{64}$/.test(asset.sha256)));
    assert.ok(manifest.assets.every((asset) => asset.size > 0));
    assert.ok(manifest.assets.every((asset) => asset.url.includes("/releases/download/v1.2.3-alpha.4/")));
    assert.equal(manifest.edge.name, "herdr-edge-1.2.3-alpha.4.mjs");
    assert.ok(/^[a-f0-9]{64}$/.test(manifest.edge.sha256));
    assert.ok(manifest.edge.size > 0);
    assert.ok(manifest.edge.url.endsWith("/releases/download/v1.2.3-alpha.4/herdr-edge-1.2.3-alpha.4.mjs"));

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

test("user can cross one runtime epoch | Given the frozen rollback baseline | When building an epoch-5 manifest | Then it declares the frozen schema and one forward migration", async () => {
  const root = await mkdtemp(join(tmpdir(), "herdr-rust-release-migration-"));
  const assetsDir = join(root, "assets");
  await mkdir(join(root, "crates", "herdr-mcp", "src"), { recursive: true });
  await mkdir(join(root, "contracts"), { recursive: true });
  await mkdir(assetsDir);
  const previous = JSON.parse(await readFile(new URL("../contracts/runtime-exec-v4.json", import.meta.url), "utf8"));
  assert.deepEqual(
    { epoch: previous.contract_epoch, hash: previous.contract_hash, tool_count: previous.tool_count },
    { epoch: 4, hash: "sha256:1f4d272cedb3334b3e17e08080793f6ed81a03dccffba2f6434f149b10e2e135", tool_count: 18 },
  );
  const current = await readFile(new URL("../contracts/runtime-exec-v5.json", import.meta.url), "utf8");
  await writeFile(join(root, "contracts", "runtime-exec-v5.json"), current);
  await writeFile(join(root, "contracts", "runtime-exec-v4.json"), JSON.stringify(previous));
  await writeFile(join(root, "crates", "herdr-mcp", "Cargo.toml"), '[package]\nversion = "1.1.0-rc.1"\n');
  const schemaPath = join(root, "crates", "herdr-mcp", "src", "state_store.rs");
  await writeFile(schemaPath, "pub const SCHEMA_VERSION: i64 = 15;\n");
  for (const target of RUST_RELEASE_TARGETS) {
    await writeFile(join(assetsDir, releaseAssetName("1.1.0-rc.1", target)), `binary-${target}`);
  }
  await writeFile(join(assetsDir, edgeReleaseAssetName("1.1.0-rc.1")), "export default {};\n");
  const options = {
    root, assetsDir, repo: "whshang/herdr-mcp", tag: "v1.1.0-rc.1",
    repositoryId: "1340180695", sourceCommit: "a".repeat(40), sourceRef: "refs/tags/v1.1.0-rc.1",
  };
  try {
    const manifest = await buildRustReleaseManifest(options);
    assert.equal(manifest.schema_version, 2);
    assert.equal(manifest.state_schema, 15);
    assert.deepEqual(manifest.contract_migration, {
      from: { epoch: 4, hash: previous.contract_hash, tool_count: 18 },
      state_schema_from: 15,
    });
    assert.equal(Object.hasOwn(manifest, "state_schema_migration"), false);
    assert.deepEqual(manifest.contract, {
      epoch: 5, hash: JSON.parse(current).contract_hash, tool_count: 18,
    });
    assert.equal(manifest.release_identity.source_ref, options.sourceRef);
    assert.equal(manifest.assets.length, RUST_RELEASE_TARGETS.length);
    await writeFile(schemaPath, "pub const SCHEMA_VERSION: i64 = 16;\n");
    const migrated = await buildRustReleaseManifest(options);
    assert.equal(migrated.state_schema, 16);
    assert.deepEqual(migrated.state_schema_migration, { from: 15, to: 16, reversible: false });
    assert.deepEqual(migrated.contract_migration, {
      from: { epoch: 4, hash: previous.contract_hash, tool_count: 18 },
      state_schema_from: 15,
    });
    for (const schema of [14, 17]) {
      await writeFile(schemaPath, `pub const SCHEMA_VERSION: i64 = ${schema};\n`);
      await assert.rejects(
        buildRustReleaseManifest(options),
        /previous frozen runtime requires state schema 15 or 16/,
      );
    }
    await writeFile(schemaPath, "pub const SCHEMA_VERSION: i64 = 15;\n");
    await writeFile(join(root, "contracts", "runtime-exec-v4.json"), JSON.stringify({ ...previous, contract_epoch: 3 }));
    await assert.rejects(buildRustReleaseManifest(options), /invalid previous frozen runtime contract/);
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
  await writeFile(join(root, "contracts", "runtime-exec-v5.json"), JSON.stringify({ contract_epoch: 3, contract_hash: "h", tool_count: 18 }));
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
    for (const target of RUST_RELEASE_TARGETS) {
      await writeFile(join(assetsDir, releaseAssetName("1.0.0", target)), `binary-${target}`);
    }
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
      /missing Edge release asset/,
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
