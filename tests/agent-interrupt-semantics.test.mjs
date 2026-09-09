import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';

const server = await readFile(new URL('../src/server.ts', import.meta.url), 'utf8');
const schema = await readFile(new URL('../src/schema.ts', import.meta.url), 'utf8');
const plannerSkill = await readFile(new URL('../assets/herdr-mcp-SKILL.md', import.meta.url), 'utf8');
const dispatchSkill = await readFile(new URL('../assets/herdr/skills/agent-dispatch/SKILL.md', import.meta.url), 'utf8');

test('working Agent stop semantics use terminal control, never a natural-language prompt', () => {
  assert.match(plannerSkill, /agent\.prompt[^\n]+has \*\*no stop\/cancel semantics\*\*/i);
  assert.match(plannerSkill, /agent\.send_keys[^\n]+"ESC"/);
  assert.match(plannerSkill, /only if the Agent is still `working`, send `keys:\["ctrl\+c"\]`/);
  assert.match(plannerSkill, /`pane\.close`\/`tab\.close` are cleanup operations after verification/);
  assert.match(dispatchSkill, /Stopping a running Agent is terminal control, not another delegation prompt/);
});

test('dynamic native method guidance exposes ESC then verify then ctrl+c', () => {
  assert.match(schema, /method === "agent\.send_keys"/);
  assert.match(schema, /keys=\[\\"ESC\\"\].+agent\.get or herdr_since.+keys=\[\\"ctrl\+c\\"\]/s);
  assert.match(schema, /agent\.prompt\/herdr_prompt never means stop\/cancel/);
  assert.match(schema, /method === "pane\.close"/);
  assert.match(schema, /Pane closure is never mutation-cancellation proof/);
});

test('working prompt remains business input and pane close is not cancellation proof', () => {
  assert.match(server, /prompt_is_not_interrupt:\s*true/);
  assert.match(server, /agent\.prompt\/herdr_prompt is business input and does not stop the current execution/);
  assert.match(server, /code:\s*"pane_close_agent_not_settled"/);
  assert.match(server, /pane_close_is_not_cancellation_proof:\s*true/);
  assert.match(server, /controlMeta\["interrupt_state_verified"\]\s*=\s*false/);
});
