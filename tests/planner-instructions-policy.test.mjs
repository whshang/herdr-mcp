import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';

const plannerSkill = await readFile(new URL('../assets/herdr-mcp-SKILL.md', import.meta.url), 'utf8');

test('user shared guidelines apply through Herdr | Given a remote planner | When project work begins | Then global instructions load before project-local instructions', () => {
  const sharedPath = '$HOME/.config/dev-guidelines/AGENTS.md';
  const sharedIndex = plannerSkill.indexOf(sharedPath);
  const projectIndex = plannerSkill.indexOf('As soon as a target project root is known');

  assert.ok(sharedIndex >= 0, 'planner policy must name the shared user guideline path');
  assert.ok(projectIndex >= 0, 'planner policy must retain project-local instruction loading');
  assert.ok(sharedIndex < projectIndex, 'shared user instructions must be loaded before project-local instructions');
  assert.match(plannerSkill, /read it in full once per conversation for that workstation\/device/);
  assert.match(plannerSkill, /apply it as the user's baseline for all projects reached through herdr-mcp/);
  assert.match(plannerSkill, /Do not substitute tool-specific generated copies/);
  assert.match(plannerSkill, /\$HOME\/\.codex\/AGENTS\.md/);
  assert.match(plannerSkill, /\$HOME\/\.pi\/agent\/AGENTS\.md/);
  assert.match(plannerSkill, /\$HOME\/\.gemini\/GEMINI\.md/);
  assert.match(plannerSkill, /Apply both layers together/);
  assert.match(plannerSkill, /surface the conflict instead of silently discarding either instruction/);
});
