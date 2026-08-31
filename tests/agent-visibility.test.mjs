/**
 * Unit tests for soft agent visibility (inspect/since allowlist).
 */
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  isAgentVisible,
  filterVisibleAgents,
  redactPaneAgents,
  agentAllowlist,
  visibilityMeta,
} from "../dist/agent-visibility.js";

const ENV_KEY = "HERDR_MCP_AGENT_ALLOW";

function withEnv(value, fn) {
  const prev = process.env[ENV_KEY];
  try {
    if (value === undefined) delete process.env[ENV_KEY];
    else process.env[ENV_KEY] = value;
    return fn();
  } finally {
    if (prev === undefined) delete process.env[ENV_KEY];
    else process.env[ENV_KEY] = prev;
  }
}

test("default visibility exposes every discovered agent", () => {
  withEnv(undefined, () => {
    assert.equal(agentAllowlist(), null);
    assert.equal(isAgentVisible("pi"), true);
    assert.equal(isAgentVisible("cline"), true);
    assert.equal(isAgentVisible("opencode"), true);
    assert.equal(isAgentVisible("anti"), true);
    assert.equal(isAgentVisible("droid"), true);
    assert.equal(isAgentVisible("grok"), true);
    assert.equal(isAgentVisible("claude"), true);
    assert.equal(isAgentVisible("omp"), true);
    assert.equal(isAgentVisible("codex"), true);
  });
});

test("HERDR_MCP_AGENT_ALLOW=* shows all", () => {
  withEnv("*", () => {
    assert.equal(agentAllowlist(), null);
    assert.equal(isAgentVisible("claude"), true);
    assert.equal(visibilityMeta(0).agent_visibility, "all");
  });
});

test("filterVisibleAgents + redactPaneAgents", () => {
  withEnv("pi", () => {
    const agents = filterVisibleAgents([
      { name: "pi", pane: "w1:p1" },
      { name: "claude", pane: "w1:p2" },
      { name: "omp", pane: "w1:p3" },
    ]);
    assert.deepEqual(agents.map((a) => a.name), ["pi"]);
    const panes = redactPaneAgents([
      { id: "w1:p1", agent: { name: "pi", status: "working" } },
      { id: "w1:p2", agent: { name: "claude", status: "working" } },
    ]);
    assert.equal(panes[0].agent?.name, "pi");
    assert.equal(panes[1].agent, null);
  });
});
