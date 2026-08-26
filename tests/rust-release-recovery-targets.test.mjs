import assert from "node:assert/strict";
import test from "node:test";
import {
  parseLegacyManifestTargets,
  parseLegacyWorkflowMatrix,
  parseTargetContract,
  resolveLegacyRecoverySource,
  resolveModernRecoveryTargets,
} from "../scripts/resolve-rust-release-recovery-targets.mjs";

const ALPHA3_WORKFLOW = `name: Rust Release
jobs:
  verify:
    runs-on: ubuntu-24.04
  build:
    needs: verify
    strategy:
      fail-fast: false
      matrix:
        include:
          - runner: macos-15
            target: aarch64-apple-darwin
          - runner: macos-15-intel
            target: x86_64-apple-darwin
          - runner: ubuntu-24.04
            target: x86_64-unknown-linux-gnu
          - runner: ubuntu-24.04-arm
            target: aarch64-unknown-linux-gnu
          - runner: windows-2025
            target: x86_64-pc-windows-msvc
    runs-on: \${{ matrix.runner }}
    steps:
      - run: cargo build
  manifest:
    needs: build
`;

const ALPHA3_BUILDER = `export const RUST_RELEASE_TARGETS = [
  "aarch64-apple-darwin",
  "x86_64-apple-darwin",
  "aarch64-unknown-linux-gnu",
  "x86_64-unknown-linux-gnu",
  "x86_64-pc-windows-msvc",
];
`;

test("modern recovery resolves the authoritative tagged target contract", () => {
  const contract = JSON.stringify({
    schema_version: 1,
    targets: [
      { runner: "macos-15", target: "aarch64-apple-darwin" },
      { runner: "windows-2025", target: "x86_64-pc-windows-msvc" },
    ],
  });
  assert.deepEqual(resolveModernRecoveryTargets(contract), {
    mode: "contract-v1",
    manifest_schema: 2,
    targets: [
      { runner: "macos-15", target: "aarch64-apple-darwin" },
      { runner: "windows-2025", target: "x86_64-pc-windows-msvc" },
    ],
  });
});

test("legacy recovery resolves an alpha.3-shaped matrix only from the same tagged sources", () => {
  const resolved = resolveLegacyRecoverySource(ALPHA3_WORKFLOW, ALPHA3_BUILDER);
  assert.equal(resolved.mode, "legacy-source");
  assert.equal(resolved.manifest_schema, 1);
  assert.deepEqual(resolved.targets, [
    { runner: "macos-15", target: "aarch64-apple-darwin" },
    { runner: "macos-15-intel", target: "x86_64-apple-darwin" },
    { runner: "ubuntu-24.04", target: "x86_64-unknown-linux-gnu" },
    { runner: "ubuntu-24.04-arm", target: "aarch64-unknown-linux-gnu" },
    { runner: "windows-2025", target: "x86_64-pc-windows-msvc" },
  ]);
});

test("legacy recovery fails closed when tagged workflow and builder disagree", () => {
  assert.throws(
    () => resolveLegacyRecoverySource(
      ALPHA3_WORKFLOW,
      ALPHA3_BUILDER.replace("aarch64-unknown-linux-gnu", "riscv64-unknown-linux-gnu"),
    ),
    /does not match the tagged manifest target set/,
  );
});

test("target parsers reject duplicates, unexpected fields, and executable expressions", () => {
  assert.throws(
    () => parseTargetContract(JSON.stringify({
      schema_version: 1,
      targets: [
        { runner: "macos-15", target: "aarch64-apple-darwin" },
        { runner: "other", target: "aarch64-apple-darwin" },
      ],
    })),
    /duplicate Rust release target/,
  );
  assert.throws(
    () => parseTargetContract(JSON.stringify({
      schema_version: 1,
      targets: [{ runner: "macos-15", target: "aarch64-apple-darwin", extra: true }],
    })),
    /only runner and target/,
  );
  assert.throws(
    () => parseLegacyManifestTargets('export const RUST_RELEASE_TARGETS = [\n  process.env.TARGET,\n];\n'),
    /unexpected legacy manifest target expression/,
  );
  assert.throws(
    () => parseLegacyWorkflowMatrix(ALPHA3_WORKFLOW.replace("target: x86_64-pc-windows-msvc", "target: aarch64-apple-darwin")),
    /duplicate Rust release target/,
  );
});
