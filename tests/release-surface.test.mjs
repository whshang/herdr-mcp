import test from "node:test";
import assert from "node:assert/strict";
import { access, readFile, stat } from "node:fs/promises";
import { constants } from "node:fs";
import { join } from "node:path";

const ROOT = new URL("../", import.meta.url).pathname;
const pkg = JSON.parse(await readFile(join(ROOT, "package.json"), "utf8"));
const lock = JSON.parse(await readFile(join(ROOT, "package-lock.json"), "utf8"));

const EXPECTED_BINS = {
  "herdr-mcp": "dist/server.js",
  "herdr-cloudflare-token": "bin/herdr-cloudflare-token",
  "herdr-cloudflare-dns-token": "bin/herdr-cloudflare-dns-token",
  "herdr-cloudflare-domain": "bin/herdr-cloudflare-domain",
  "herdr-custom-domain-cutover": "bin/herdr-custom-domain-cutover",
  "herdr-extension-host": "bin/herdr-extension-host",
  "herdr-runtime-generation": "bin/herdr-runtime-generation",
  "herdr-self-update": "bin/herdr-self-update",
};

test("package and lock expose the complete public CLI surface", () => {
  assert.deepEqual(pkg.bin, EXPECTED_BINS);
  assert.deepEqual(lock.packages?.[""]?.bin, EXPECTED_BINS);
  assert.equal(pkg.version, lock.packages?.[""]?.version);
});

test("non-node public CLI files are executable", async () => {
  for (const [name, rel] of Object.entries(EXPECTED_BINS)) {
    if (rel === "dist/server.js") continue;
    const path = join(ROOT, rel);
    await access(path, constants.X_OK);
    const mode = (await stat(path)).mode;
    assert.notEqual(mode & 0o111, 0, `${name} must be executable`);
  }
});

test("CI/CD and documentation publishing entrypoints are tracked in the release tree", async () => {
  for (const rel of [
    ".github/workflows/ci.yml",
    ".github/workflows/pages.yml",
    ".github/workflows/cloudflare-edge.yml",
    "scripts/release-gate.sh",
    "scripts/sign-macos-release.sh",
    "scripts/build-site.mjs",
    "assets/herdr-mcp-SKILL.md",
    "site/index.html",
  ]) {
    await access(join(ROOT, rel), constants.R_OK);
  }
  assert.equal(pkg.scripts?.["build:site"], "node scripts/build-site.mjs");
  assert.equal(pkg.scripts?.["self:update"], "bin/herdr-self-update");
});

test("Actions consume the shared gate and keep Cloudflare deployment on the gated Worker plane", async () => {
  const ci = await readFile(join(ROOT, ".github/workflows/ci.yml"), "utf8");
  const gate = await readFile(join(ROOT, "scripts/release-gate.sh"), "utf8");
  const pages = await readFile(join(ROOT, ".github/workflows/pages.yml"), "utf8");
  const edge = await readFile(join(ROOT, ".github/workflows/cloudflare-edge.yml"), "utf8");
  assert.match(ci, /scripts\/release-gate\.sh rust/);
  assert.match(ci, /scripts\/release-gate\.sh node/);
  assert.match(ci, /scripts\/release-gate\.sh hygiene/);
  assert.match(gate, /npm run build:site/);
  assert.match(gate, /extension_smoke\.mjs/);
  assert.match(pages, /npm run build:site/);
  assert.match(pages, /path: site-dist/);
  assert.match(pages, /actions\/deploy-pages@v4/);
  assert.match(edge, /cloudflare\/wrangler-action@v4/);
  assert.match(edge, /environment: production/);
  assert.match(edge, /wranglerVersion: "4"/);
  assert.match(edge, /wrangler\.prod\.toml/);
  assert.match(edge, /!edge\/cloudflare\/\*\*\/\*\.md/, "Edge docs-only changes must not redeploy the production Worker");
  assert.doesNotMatch(edge, /cloudflared|DNS Write|Tunnel/);
});

test("Rust release verification consumes the shared gate with bounded runtime cleanup", async () => {
  const release = await readFile(join(ROOT, ".github/workflows/rust-release.yml"), "utf8");
  const gate = await readFile(join(ROOT, "scripts/release-gate.sh"), "utf8");
  assert.match(release, /scripts\/release-gate\.sh full/);
  const start = gate.indexOf("scripts/ci-herdr-runtime.sh start");
  const rootTests = gate.indexOf("npm test");
  const stop = gate.lastIndexOf("scripts/ci-herdr-runtime.sh stop");
  assert.ok(start >= 0, "shared gate must start the pinned Herdr runtime");
  assert.ok(rootTests > start, "runtime must be ready before root transport tests");
  assert.ok(stop > rootTests, "runtime cleanup must run after root transport tests");
  assert.match(gate, /trap cleanup EXIT/, "shared gate must clean the isolated runtime on failure");
});

test("shared gate scrubs production overrides and isolates the live Herdr test runtime", async () => {
  const gate = await readFile(join(ROOT, "scripts/release-gate.sh"), "utf8");
  for (const name of [
    "HERDR_MCP_PORT",
    "HERDR_MCP_CONFIG_DIR",
    "HERDR_MCP_INSTANCE",
    "HERDR_EXTENSION_PATH",
    "HERDR_EXTENSION_ORIGIN",
    "HERDR_MCP_ROOT",
  ]) {
    assert.match(gate, new RegExp(`\\b${name}\\b`));
  }
  assert.match(gate, /mktemp -d \/tmp\/herdr-gate/);
  assert.match(gate, /XDG_CONFIG_HOME=/);
  assert.match(gate, /HERDR_STATE_DIR=/);
  assert.match(gate, /HERDR_SOCKET_PATH=/);
  assert.match(gate, /HERDR_INSTALL_DIR=/);
});

test("pinned Herdr bootstrap supports CI Linux and local macOS", async () => {
  const bootstrap = await readFile(join(ROOT, "scripts/ci-herdr-runtime.sh"), "utf8");
  assert.match(bootstrap, /herdr-linux-x86_64/);
  assert.match(bootstrap, /herdr-linux-aarch64/);
  assert.match(bootstrap, /herdr-macos-aarch64/);
  assert.match(bootstrap, /herdr-macos-x86_64/);
  assert.match(bootstrap, /local start requires isolated XDG_CONFIG_HOME/);
  assert.match(bootstrap, /HERDR_STATE_DIR must equal XDG_CONFIG_HOME\/herdr/);
  assert.match(bootstrap, /HERDR_SOCKET must equal HERDR_STATE_DIR\/herdr.sock/);
  assert.match(bootstrap, /server_pid_identity_ok/);
  assert.match(bootstrap, /terminate_exact_server_pid/);
  assert.match(bootstrap, /workspace create exceeded 10s/);
  assert.match(bootstrap, /HERDR_CI_WORKSPACE/);
  assert.match(bootstrap, /CI_WORKSPACE=.*STATE_DIR.*ci-workspace/);
  assert.doesNotMatch(
    bootstrap,
    /HERDR_BIN.*server stop/,
    "isolated cleanup must never invoke generic Herdr server stop",
  );
  const gate = await readFile(join(ROOT, "scripts/release-gate.sh"), "utf8");
  const ownership = gate.indexOf("RUNTIME_STARTED=1");
  const start = gate.indexOf("scripts/ci-herdr-runtime.sh start");
  assert.ok(ownership >= 0 && start > ownership, "cleanup ownership must be marked before start can fail");
});

test("Herdr dependency recovery stays internal and is installed on both install and updater paths", async () => {
  const cli = await readFile(join(ROOT, "crates/herdr-mcp/src/cli.rs"), "utf8");
  const supervisor = await readFile(join(ROOT, "crates/herdr-mcp/src/herdr_supervisor.rs"), "utf8");
  const updater = await readFile(join(ROOT, "crates/herdr-mcp/src/updater.rs"), "utf8");
  const runtimeMeta = await readFile(join(ROOT, "crates/herdr-mcp/src/runtime_meta.rs"), "utf8");
  assert.match(cli, /herdr-mcp herdr-supervisor/);
  assert.match(supervisor, /dev\.herdr-mcp\.herdr-supervisor|DEFAULT_HERDR_SUPERVISOR_LABEL/);
  assert.match(supervisor, /desired_running/);
  assert.match(supervisor, /session_restore_blocked/);
  assert.match(supervisor, /KeepAlive/);
  assert.match(supervisor, /ThrottleInterval/);
  assert.doesNotMatch(supervisor, /"StartInterval"/);
  assert.match(updater, /service_lifecycle::run\(ServiceCommand::Install/);
  assert.match(runtimeMeta, /MIGRATED_TOOLS: \[&str; 18\]/);
  assert.doesNotMatch(runtimeMeta, /herdr_supervisor/);
});

test("request child lifecycle persists only ownership metadata and reaps confirmed boot orphans", async () => {
  const children = await readFile(join(ROOT, "crates/herdr-mcp/src/child_process.rs"), "utf8");
  const service = await readFile(join(ROOT, "crates/herdr-mcp/src/service_manager.rs"), "utf8");
  const main = await readFile(join(ROOT, "crates/herdr-mcp/src/main.rs"), "utf8");
  const status = await readFile(join(ROOT, "crates/herdr-mcp/src/status.rs"), "utf8");
  assert.match(service, /HERDR_MCP_CHILD_REGISTRY/);
  assert.match(main, /reap_confirmed_orphans_on_boot/);
  assert.match(status, /child_process::doctor_line/);
  assert.match(children, /process_start_identity_mismatch/);
  assert.match(children, /process_command_identity_mismatch/);
  assert.match(children, /recorded_parent_still_alive/);
  assert.match(children, /record_too_old_for_automatic_reap/);
  assert.match(children, /child-process-reap-last\.json/);
  assert.doesNotMatch(children, /get_args\(/, "persistent child ownership must not record request arguments");
});

test("service lifecycle keeps Herdr supervisor transactional across install update rollback and uninstall", async () => {
  const main = await readFile(join(ROOT, "crates/herdr-mcp/src/main.rs"), "utf8");
  const updater = await readFile(join(ROOT, "crates/herdr-mcp/src/updater.rs"), "utf8");
  const lifecycle = await readFile(join(ROOT, "crates/herdr-mcp/src/service_lifecycle.rs"), "utf8");
  const supervisor = await readFile(join(ROOT, "crates/herdr-mcp/src/herdr_supervisor.rs"), "utf8");
  assert.match(main, /Command::Service\(command\) => service_lifecycle::run\(command\)/);
  assert.match(updater, /service_lifecycle::run\(ServiceCommand::Install/);
  assert.doesNotMatch(updater, /herdr_supervisor::ensure_installed_for_service/);
  assert.match(lifecycle, /InstallRecovery::Rollback => service_manager::run\(ServiceCommand::Rollback\)/);
  assert.match(lifecycle, /InstallRecovery::Uninstall => service_manager::run\(ServiceCommand::Uninstall\)/);
  assert.match(lifecycle, /reconcile_after_service_rollback/);
  assert.match(lifecycle, /remove_for_service\(\)\?/);
  assert.match(supervisor, /refusing bootout because it may own a live Herdr child/);
  assert.match(supervisor, /runtime_supports_supervisor/);
  assert.match(supervisor, /starts_with\("herdr-mcp herdr-supervisor </);
  assert.match(lifecycle, /rollback_target_runtime_binary/);
  assert.match(lifecycle, /RollbackSupervisorStrategy::Preserve/);
});

test("current workflows pin checkout and setup-node to reviewed v7 commits", async () => {
  const checkout = "3d3c42e5aac5ba805825da76410c181273ba90b1";
  const setupNode = "820762786026740c76f36085b0efc47a31fe5020";
  for (const rel of [
    ".github/workflows/ci.yml",
    ".github/workflows/pages.yml",
    ".github/workflows/cloudflare-edge.yml",
    ".github/workflows/rust-release.yml",
    ".github/workflows/rust-release-recover.yml",
  ]) {
    const workflow = await readFile(join(ROOT, rel), "utf8");
    for (const match of workflow.matchAll(/actions\/checkout@([^\s]+)/g)) {
      assert.equal(match[1], checkout, `${rel} checkout ref must be pinned`);
    }
    for (const match of workflow.matchAll(/actions\/setup-node@([^\s]+)/g)) {
      assert.equal(match[1], setupNode, `${rel} setup-node ref must be pinned`);
    }
  }
});

test("Rust release defaults to one authoritative macOS ARM64 + Windows x64 target contract", async () => {
  const release = await readFile(join(ROOT, ".github/workflows/rust-release.yml"), "utf8");
  const targetContract = JSON.parse(await readFile(join(ROOT, ".github/rust-release-targets.json"), "utf8"));
  assert.deepEqual(targetContract, {
    schema_version: 1,
    targets: [
      { runner: "macos-15", target: "aarch64-apple-darwin" },
      { runner: "windows-2025", target: "x86_64-pc-windows-msvc" },
    ],
  });
  assert.match(release, /Load authoritative Rust release targets/);
  assert.match(release, /\.github\/rust-release-targets\.json/);
  assert.match(release, /needs: \[verify, targets\]/);
  assert.match(release, /matrix: \$\{\{ fromJSON\(needs\.targets\.outputs\.matrix\) \}\}/);
  assert.doesNotMatch(release, /x86_64-apple-darwin/);
  assert.doesNotMatch(release, /unknown-linux-gnu/);
});

test("tagged macOS Rust releases require a stable Developer ID code identity", async () => {
  const release = await readFile(join(ROOT, ".github/workflows/rust-release.yml"), "utf8");
  const signer = await readFile(join(ROOT, "scripts/sign-macos-release.sh"), "utf8");
  assert.match(release, /Sign tagged macOS release identity/);
  assert.match(release, /startsWith\(github\.ref, 'refs\/tags\/v'\)/);
  assert.match(release, /contains\(matrix\.target, 'apple-darwin'\)/);
  assert.match(release, /HERDR_MACOS_CERT_P12_BASE64: \${{ secrets\.HERDR_MACOS_CERT_P12_BASE64 }}/);
  assert.match(release, /HERDR_MACOS_CERT_PASSWORD: \${{ secrets\.HERDR_MACOS_CERT_PASSWORD }}/);
  assert.match(release, /HERDR_MACOS_SIGNING_IDENTITY: \${{ secrets\.HERDR_MACOS_SIGNING_IDENTITY }}/);
  assert.match(release, /HERDR_MACOS_TEAM_ID: \${{ vars\.HERDR_MACOS_TEAM_ID }}/);
  assert.match(release, /scripts\/sign-macos-release\.sh/);
  assert.match(signer, /STABLE_IDENTIFIER="dev\.herdr\.mcp"/);
  assert.match(signer, /--options runtime/);
  assert.match(signer, /--timestamp/);
  assert.match(signer, /--identifier "\$STABLE_IDENTIFIER"/);
  assert.match(signer, /TeamIdentifier is missing/);
  assert.match(signer, /does not match expected/);
  assert.match(signer, /designated requirement is still cdhash-bound/);
});

test("Rust GitHub Release provenance is tag-only and fail-closed before publish", async () => {
  const release = await readFile(join(ROOT, ".github/workflows/rust-release.yml"), "utf8");
  const attestJob = release.indexOf("\n  attest:\n");
  const publishJob = release.indexOf("\n  publish:\n");
  assert.ok(attestJob >= 0, "release workflow must have a dedicated attestation job");
  assert.ok(publishJob > attestJob, "attestation must be declared before GitHub Release publishing");
  const attest = release.slice(attestJob, publishJob);
  const publish = release.slice(publishJob);
  assert.match(attest, /needs: manifest/);
  assert.match(attest, /if: startsWith\(github\.ref, 'refs\/tags\/v'\)/);
  assert.match(attest, /contents: read/);
  assert.match(attest, /id-token: write/);
  assert.match(attest, /attestations: write/);
  assert.match(attest, /artifact-metadata: write/);
  assert.match(
    attest,
    /uses: actions\/attest@1e69f48acb82d1966a394da916b4c1698aa569d6 # v4\.2\.2/,
    "release provenance action must be pinned to the reviewed v4.2.2 commit",
  );
  assert.match(attest, /release-assets\/herdr-mcp-\*/);
  assert.match(attest, /release-assets\/release-manifest\.json/);
  assert.doesNotMatch(release, /pack-extension\.mjs/);
  assert.doesNotMatch(release, /Pack browser extension release zip/);
  assert.match(release, /--repository-id \"\$GITHUB_REPOSITORY_ID\"/);
  assert.match(release, /--source-commit \"\$GITHUB_SHA\"/);
  assert.match(release, /--source-ref \"\$GITHUB_REF\"/);
  assert.match(release, /--workflow-name \"\$GITHUB_WORKFLOW\"/);
  assert.match(publish, /needs: attest/, "GitHub Release publishing must fail closed when attestation fails");
  assert.match(publish, /if: startsWith\(github\.ref, 'refs\/tags\/v'\)/);
  assert.match(publish, /Verify immutable release identity/);
  assert.match(publish, /manifest source_commit does not match tag commit/);
  assert.match(
    publish,
    /-name 'herdr-mcp-\*'/,
    "publish identity verify must compare the complete runtime bundle directly",
  );
  assert.match(publish, /refusing publish overwrite/, "publish must fail closed when GitHub Release already exists");
  assert.doesNotMatch(publish, /--clobber/, "tag publish must not clobber existing release assets");
  assert.match(
    publish,
    /GH_REPO: \$\{\{ github\.repository \}\}/,
    "publish must name the GitHub repository explicitly because the job does not checkout git metadata",
  );
  assert.match(publish, /release_flags=\(--verify-tag --generate-notes\)/);
  assert.match(publish, /release_flags\+=\(--prerelease\)/, "semver prereleases must be marked as GitHub prereleases");
  assert.doesNotMatch(attest, /workflow_dispatch/);
  assert.doesNotMatch(publish, /workflow_dispatch/);
});
test("Rust Release recovery republishes only a previously attested GitHub run", async () => {
  const recovery = await readFile(join(ROOT, ".github/workflows/rust-release-recover.yml"), "utf8");
  assert.match(recovery, /workflow_dispatch:/);
  assert.match(recovery, /source_run_id:/);
  assert.match(recovery, /tag:/);
  assert.match(recovery, /actions: read/);
  assert.match(recovery, /contents: write/);
  assert.match(recovery, /attestations: read/);
  assert.match(recovery, /run-id: \$\{\{ inputs\.source_run_id \}\}/);
  assert.match(recovery, /name: rust-release-bundle/);
  assert.match(recovery, /\.github\/workflows\/rust-release\.yml/);
  assert.match(recovery, /\.github\/rust-release-targets\.json/);
  assert.match(recovery, /resolve-rust-release-recovery-targets\.mjs/);
  assert.match(recovery, /git cat-file -e "\$\{source_digest\}:\.github\/rust-release-targets\.json"/);
  assert.match(recovery, /git show "\$\{source_digest\}:\.github\/workflows\/rust-release\.yml"/);
  assert.match(recovery, /git show "\$\{source_digest\}:scripts\/build-rust-release-manifest\.mjs"/);
  assert.match(recovery, /source_target_mode/);
  assert.match(recovery, /source_manifest_schema/);
  assert.match(recovery, /source_target_matrix/);
  assert.match(recovery, /source run build matrix does not match tagged source targets/);
  assert.match(recovery, /manifest\.get\(\"schema_version\"\) != source_manifest_schema/);
  assert.match(recovery, /source_manifest_schema == 2/);
  assert.match(recovery, /release manifest source identity mismatch/);
  assert.match(recovery, /release manifest repository identity mismatch/);
  assert.match(recovery, /release manifest provenance identity mismatch/);
  assert.match(recovery, /release manifest targets do not match tagged target contract/);
  assert.doesNotMatch(recovery, /herdr-mcp-extension-/);
  assert.match(recovery, /release_asset_count=/);
  assert.match(recovery, /steps\.verify\.outputs\.release_asset_count/);
  assert.doesNotMatch(recovery, /length == 5/);
  assert.doesNotMatch(recovery, /== \"6\"/);
  assert.match(recovery, /head_sha/);
  assert.match(recovery, /conclusion==\"success\"/);
  assert.match(recovery, /gh attestation verify/);
  assert.match(recovery, /--signer-workflow/);
  assert.match(recovery, /--source-ref/);
  assert.match(recovery, /--source-digest/);
  assert.match(recovery, /--deny-self-hosted-runners/);
  assert.match(recovery, /gh release create/);
  assert.match(recovery, /--verify-tag/);
  assert.match(recovery, /--prerelease/);
});

test("local Cloudflare/Pages build state is excluded from the published package", async () => {
  const rootIgnore = await readFile(join(ROOT, ".gitignore"), "utf8");
  const edgeIgnore = await readFile(join(ROOT, "edge/cloudflare/.gitignore"), "utf8");
  assert.match(rootIgnore, /edge\/cloudflare\/\.wrangler\//);
  assert.match(rootIgnore, /site-dist\//);
  assert.match(rootIgnore, /docs\/_wip\//);
  assert.match(edgeIgnore, /^\.wrangler\/$/m);
});
