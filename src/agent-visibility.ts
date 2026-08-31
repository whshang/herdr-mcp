/**
 * Soft visibility filter for web-facing agent lists (inspect / since).
 * Hidden agents are omitted from listings only — herdr_prompt / herdr_call still work.
 */

const DEFAULT_ALLOW: readonly string[] = [];

/** Parsed allowlist, or null when every agent is visible (`*` / `all`). */
export function agentAllowlist(): Set<string> | null {
  const raw = process.env.HERDR_MCP_AGENT_ALLOW;
  if (raw !== undefined) {
    const trimmed = raw.trim();
    if (trimmed === "*" || trimmed.toLowerCase() === "all") return null;
    if (trimmed === "") return new Set(); // explicit empty = hide all from lists
    return new Set(
      trimmed.split(",").map((s) => s.trim().toLowerCase()).filter(Boolean),
    );
  }
  return null;
}

export function defaultAgentAllowlist(): readonly string[] {
  return DEFAULT_ALLOW;
}

/** True if this agent name/kind should appear in web-facing lists. */
export function isAgentVisible(name: unknown, kind?: unknown): boolean {
  const allow = agentAllowlist();
  if (allow === null) return true;
  const candidates = [name, kind]
    .filter((x): x is string => typeof x === "string" && x.trim().length > 0)
    .map((s) => s.trim().toLowerCase());
  if (candidates.length === 0) return false;
  for (const c of candidates) {
    if (allow.has(c)) return true;
    // "pi-agent" / "opencode_cli" still match token "pi" / "opencode" as whole segment
    for (const token of allow) {
      if (c === token || c.startsWith(token + "-") || c.startsWith(token + "_")) return true;
    }
  }
  return false;
}

export type AgentLike = {
  name?: unknown;
  agent?: unknown;
  kind?: unknown;
};

/** Filter agents[] for MCP inspect/since. */
export function filterVisibleAgents<T>(agents: readonly T[]): T[] {
  return agents.filter((a) => {
    const rec = a as AgentLike;
    const name = rec.name ?? rec.agent;
    return isAgentVisible(name, rec.kind);
  });
}

/**
 * For panes[] entries shaped like { agent: { name, ... } | null }:
 * clear nested agent when not on the allowlist (pane itself stays).
 */
export function redactPaneAgents<T extends { agent?: { name?: unknown; kind?: unknown } | null }>(
  panes: T[],
): T[] {
  return panes.map((p) => {
    const ag = p.agent;
    if (!ag || typeof ag !== "object") return p;
    if (isAgentVisible(ag.name, ag.kind)) return p;
    return { ...p, agent: null };
  });
}

export function visibilityMeta(hiddenCount: number): Record<string, unknown> {
  const allow = agentAllowlist();
  if (allow === null) {
    return { agent_visibility: "all", agents_hidden: 0 };
  }
  return {
    agent_visibility: "allowlist",
    agent_allow: [...allow].sort(),
    agents_hidden: hiddenCount,
    hint: "HERDR_MCP_AGENT_ALLOW explicitly restricts discovery. Unset, '*' or 'all' shows every discovered agent; roles and quality are not inferred from agent names.",
  };
}
