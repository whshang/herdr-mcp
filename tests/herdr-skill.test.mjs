import test from "node:test";
import assert from "node:assert/strict";
import { herdrSkillPointer, fetchHerdrSkill, HERDR_SKILL_BUNDLED } from "../dist/herdr-skill.js";

test("herdrSkillPointer exposes upstream and bundled sources", () => {
  const p = herdrSkillPointer();
  assert.equal(p.tool, "herdr_skill");
  assert.match(p.upstream, /herdrdev\/herdr\/master\/skills\/herdr\/SKILL\.md/);
  assert.equal(p.bundled, HERDR_SKILL_BUNDLED);
  assert.match(p.hint, /bundled/);
});

test("fetchHerdrSkill offline mode returns bundled content", async () => {
  const prev = process.env.HERDR_SKILL_NETWORK;
  process.env.HERDR_SKILL_NETWORK = "0";
  try {
    const r = await fetchHerdrSkill({ refresh: true });
    assert.equal(r.ok, true);
    assert.equal(r.origin, "bundled");
    assert.match(r.content, /^---\s*\nname: herdr/m);
    assert.ok(r.bytes > 1000);
  } finally {
    if (prev === undefined) delete process.env.HERDR_SKILL_NETWORK;
    else process.env.HERDR_SKILL_NETWORK = prev;
  }
});
