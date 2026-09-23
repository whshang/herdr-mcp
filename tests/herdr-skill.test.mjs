import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import {
  herdrSkillPointer,
  fetchHerdrSkill,
  HERDR_MCP_SKILL_BUNDLED,
} from "../dist/herdr-skill.js";

const plannerSkillSource = readFileSync(new URL("../assets/herdr-mcp-SKILL.md", import.meta.url), "utf8");

test("herdrSkillPointer exposes project policy, native reference and self-update entrypoint", () => {
  const p = herdrSkillPointer();
  assert.equal(p.tool, "herdr_skill");
  assert.match(p.project_upstream, /whshang\.github\.io\/herdr-mcp\/herdr-mcp-SKILL\.md/);
  assert.equal(p.project_bundled, HERDR_MCP_SKILL_BUNDLED);
  assert.equal(p.native_reference, "local:herdr --skill");
  assert.equal(p.self_update, "herdr-self-update");
  assert.match(p.hint, /Remote-planner policy first/);
});

test("fetchHerdrSkill offline mode returns bundled project policy plus live runtime context", async () => {
  const prev = process.env.HERDR_SKILL_NETWORK;
  process.env.HERDR_SKILL_NETWORK = "0";
  try {
    const r = await fetchHerdrSkill({ refresh: true, includeNativeReference: false });
    assert.equal(r.ok, true);
    assert.equal(r.project_skill.origin, "bundled");
    assert.equal(r.project_skill.source, HERDR_MCP_SKILL_BUNDLED);
    assert.match(r.content, /# Herdr-MCP remote planner/);
    assert.match(r.content, /AGENTS\.md.*CLAUDE\.md.*README\.md/s);
    assert.match(r.content, /\.agents\/skills\/\*\/SKILL\.md/);
    assert.doesNotMatch(r.content, /\.claude\/skills\/\*\/SKILL\.md/);
    assert.match(r.content, /Connected-tool calling contract/);
    assert.match(r.content, /`device`.*inside the tool arguments/s);
    assert.match(r.content, /`params` as a \*\*JSON object string\*\*/);
    assert.match(r.content, /`command` \*\*or\*\* `steps`, never both/);
    assert.match(r.content, /`confirm_dirty`.*`confirm_busy`.*acknowledge observed state only/s);
    assert.match(r.content, /stable `idempotency_key`/);
    assert.match(r.content, /Existing file: prefer `herdr_fs_edit` or `herdr_fs_patch`/);
    assert.match(r.content, /Start once with `herdr_exec_start`/);
    assert.match(r.content, /`delivery_state=not_delivered`/);
    assert.match(r.content, /one logical intent, one authority boundary, and at most one mutation boundary/);
    assert.match(r.content, /`herdr_since\(cursor\)`/);
    assert.match(r.content, /A host-side rejection with no Herdr execution\/result identity is not evidence/);
    assert.match(r.content, /A process exit code, Agent `done`, or prose claim is not task completion/);
    assert.match(r.content, /device -> project\/workspace -> continuity\/history -> live Git\/runtime/);
    assert.match(r.content, /project `AGENTS\.md` owns version-specific runtime, release, CI\/CD, browser-extension, Automation Client, DEV\/PROD/);
    assert.match(r.content, /completion sweep/);
    assert.match(r.content, /Live herdr-mcp runtime context/);
    assert.doesNotMatch(r.content, /dev\.herdr-mcp\.health-watchdog/);
    assert.doesNotMatch(r.content, /grant_type=client_credentials/);
    assert.doesNotMatch(r.content, /HERDR_MCP_TOKEN/);
    assert.ok(plannerSkillSource.split("\n").length <= 150, "main Skill should stay thin enough for eager loading");
    assert.ok(Buffer.byteLength(plannerSkillSource, "utf8") < 12000, "main Skill should stay below 12 KiB");
    assert.equal(r.runtime.contract_profile, process.env.HERDR_MCP_CONTRACT_PROFILE || "current");
    assert.equal(r.runtime.build_commit, process.env.HERDR_MCP_BUILD_COMMIT || null);
    assert.equal(r.runtime.active_runtime.source_commit, process.env.HERDR_MCP_BUILD_COMMIT || null);
    assert.equal(r.runtime.active_runtime.truth_source, "active_binary+runtime_generation_manager");
    assert.equal(typeof r.runtime.worker_fallbacks, "object");
    assert.equal(r.runtime.worker_fallbacks.dsh_headless.invocation, "herdr_exec_start -> dsh --profile headless <task>");
    assert.equal(r.runtime.worker_fallbacks.dsh_tui.role, "human-interactive fallback");
    assert.equal(r.native_reference, undefined);
    assert.ok(r.bytes > 3000);
  } finally {
    if (prev === undefined) delete process.env.HERDR_SKILL_NETWORK;
    else process.env.HERDR_SKILL_NETWORK = prev;
  }
});

test("agent-dispatch skill keeps host policy external to worker selection", () => {
  const content = readFileSync(
    new URL("../assets/herdr/skills/agent-dispatch/SKILL.md", import.meta.url),
    "utf8",
  );
  assert.match(content, /External host outcome/);
  assert.match(content, /no Herdr execution identity or result fields/);
  assert.match(content, /Host policy is external to Agent Dispatch/);
  assert.match(content, /stable `idempotency_key`/);
  assert.match(content, /delivery evidence says `not_delivered`/);
  assert.doesNotMatch(content, /one bounded, identical retry/);
  assert.doesNotMatch(content, /existing compatible local Agent/);
  assert.doesNotMatch(content, /fallback chain stops/);
});

test("user gets explicit connected-tool shapes | Given progressive Herdr Skills | When domain contracts are read | Then device, params, execution, confirmation, and idempotency rules are explicit", () => {
  const workstation = readFileSync(
    new URL("../assets/herdr/skills/workstation-control/SKILL.md", import.meta.url),
    "utf8",
  );
  const execution = readFileSync(
    new URL("../assets/herdr/skills/execution/SKILL.md", import.meta.url),
    "utf8",
  );
  const mutation = readFileSync(
    new URL("../assets/herdr/skills/files-mutation/SKILL.md", import.meta.url),
    "utf8",
  );
  const orchestration = readFileSync(
    new URL("../assets/herdr/skills/development-orchestration/SKILL.md", import.meta.url),
    "utf8",
  );
  const paneLocal = readFileSync(new URL("../assets/herdr-agent-SKILL.md", import.meta.url), "utf8");

  assert.match(workstation, /Put `device` inside the public tool arguments object/);
  assert.match(workstation, /`params` as a \*\*JSON object string\*\*/);
  assert.doesNotMatch(workstation, /herdr_call\([^\n]+params=\{/);
  assert.match(workstation, /Bare workspace\/pane ids are scoped/);
  assert.match(execution, /`command` \*\*or\*\* `steps`, never both/);
  assert.match(execution, /include the exact `project_root`/);
  assert.match(mutation, /Read the target\/diff before setting `confirm_dirty` or `confirm_busy`/);
  assert.match(mutation, /Never use `herdr_fs_write` as a shortcut/);
  assert.match(orchestration, /Herdr-MCP development-only retrospective/);
  assert.match(paneLocal, /pane-local only.*remote\/Web planner.*progressive Skills/s);
});

test("fetchHerdrSkill appends release-matched native Herdr reference with remote-scope warning", async () => {
  const prev = process.env.HERDR_SKILL_NETWORK;
  process.env.HERDR_SKILL_NETWORK = "0";
  try {
    const r = await fetchHerdrSkill({ refresh: true, includeNativeReference: true });
    assert.equal(r.ok, true);
    assert.ok(r.native_reference);
    assert.match(r.native_reference.source, /^(local:herdr --skill|bundled:assets\/herdr-agent-SKILL\.md)$/);
    assert.match(r.content, /Appendix: release-matched native Herdr reference/);
    assert.match(r.content, /HERDR_ENV=1.*does \*\*not\*\* override/s);
    assert.match(r.content, /engineering-robustness/);
    assert.match(r.content, /workstation-control/);
    assert.match(r.content, /remote\/Web planner.*progressive Skills/s);
    assert.match(r.content, /name: herdr/);
  } finally {
    if (prev === undefined) delete process.env.HERDR_SKILL_NETWORK;
    else process.env.HERDR_SKILL_NETWORK = prev;
  }
});
