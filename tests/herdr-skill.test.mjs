import test from "node:test";
import assert from "node:assert/strict";
import {
  herdrSkillPointer,
  fetchHerdrSkill,
  HERDR_MCP_SKILL_BUNDLED,
} from "../dist/herdr-skill.js";

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
    assert.match(r.content, /# herdr-mcp remote planner skill/);
    assert.match(r.content, /Direct workstation operations first/);
    assert.match(r.content, /AGENTS\.md.*CLAUDE\.md.*README\.md/s);
    assert.match(r.content, /including read-only analysis/);
    assert.match(r.content, /\.agents\/skills\/\*\/SKILL\.md/);
    assert.doesNotMatch(r.content, /\.claude\/skills\/\*\/SKILL\.md/);
    assert.match(r.content, /Prefer project-scoped skills over same-name user-scoped skills/);
    assert.match(r.content, /herdr-self-update apply/);
    assert.match(r.content, /Do not treat exit code 0 alone as completion evidence/);
    assert.match(r.content, /Control-plane outage recovery/);
    assert.match(r.content, /RunAtLoad=true.*KeepAlive=true/s);
    assert.match(r.content, /dev\.herdr-mcp\.health-watchdog/);
    assert.match(r.content, /historical `dev\.herdr-mcp\.watchdog` identity/);
    assert.match(r.content, /health-watchdog\.\*/);
    assert.match(r.content, /5 seconds.*10 seconds.*20 seconds/s);
    assert.match(r.content, /roughly.*35 seconds/s);
    assert.match(r.content, /exactly three.*read-only.*reconnect attempts/s);
    assert.match(r.content, /bounded three-retry recovery window/s);
    assert.match(r.content, /agent_status_wait_timeout.*not.*offline/s);
    assert.match(r.content, /boot_id.*herdr_since\(cursor=0\)/s);
    assert.match(r.content, /never blindly resend it/s);
    assert.match(r.content, /Live herdr-mcp runtime context/);
    assert.match(r.content, /Latency-aware tool scheduling/);
    assert.match(r.content, /dependency-aware \*\*wave\*\*/);
    assert.match(r.content, /herdr_git status.*diff.*log.*herdr_exec.*herdr_fs_grep.*compacted/s);
    assert.match(r.content, /counts.*compacted.*summarized `output`/s);
    assert.match(r.content, /Long build\/test\/process work belongs in `herdr_exec_start` \/ `herdr_exec_read`/);
    assert.match(r.content, /herdr_exec_read\(offset=next_offset\)/);
    assert.match(r.content, /prefer `herdr_exec_start` -> `herdr_exec_read` \(delta\) over a blocking `herdr_exec`/);
    assert.match(r.content, /phase=started/);
    assert.match(r.content, /phase=completed/);
    assert.match(r.content, /progress.*bytes_read.*bytes_total.*elapsed_ms/s);
    assert.match(r.content, /completion resource sweep/);
    assert.match(r.content, /Do not wait for the user to notice accumulated panes/);
    assert.match(r.content, /settled Agent.*does not need to remain open.*preserve task history/s);
    assert.match(r.content, /canonical reusable `herdr-mcp:utility` pane/);
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
    assert.match(r.content, /silent-wrongness/);
    assert.match(r.content, /state as separate planes/);
    assert.match(r.content, /continuity\.search/);
    assert.match(r.content, /confirmation_required/);
    assert.match(r.content, /Do not ask the user to provide a Herdr continuity ID before attempting safe discovery/);
    assert.match(r.content, /Never.*newest.*textually most similar/s);
    assert.match(r.content, /name: herdr/);
  } finally {
    if (prev === undefined) delete process.env.HERDR_SKILL_NETWORK;
    else process.env.HERDR_SKILL_NETWORK = prev;
  }
});
