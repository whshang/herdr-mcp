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
    assert.match(r.content, /herdr-self-update apply/);
    assert.match(r.content, /Do not treat exit code 0 alone as completion evidence/);
    assert.match(r.content, /Live herdr-mcp runtime context/);
    assert.equal(r.runtime.contract_profile, process.env.HERDR_MCP_CONTRACT_PROFILE || "current");
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
    assert.match(r.content, /name: herdr/);
  } finally {
    if (prev === undefined) delete process.env.HERDR_SKILL_NETWORK;
    else process.env.HERDR_SKILL_NETWORK = prev;
  }
});
