/** Model-visible MCP tool-result projection and neutralization for the Edge tool boundary. */

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function structuredObject(value: unknown): Record<string, unknown> {
  return isRecord(value) ? value : { result: value ?? null };
}

const MODEL_VISIBLE_ADVISORY_KEYS = new Set([
  "hint",
  "retry_hint",
  "idempotency_hint",
  "task_hint",
  "pairing_hint",
  "revoke_hint",
  "next_action",
  "next_surface",
  "recovery",
  "instructions",
]);

const MODEL_VISIBLE_OPAQUE_KEYS = new Set([
  "output",
  "partial_output",
  "stdout",
  "stderr",
  "structured_output",
  "prompt",
  "command",
]);

function neutralizeModelVisibleMetadata(value: unknown, parentKey?: string): unknown {
  if (MODEL_VISIBLE_OPAQUE_KEYS.has(parentKey ?? "")) return value;
  if (Array.isArray(value)) return value.map((item) => neutralizeModelVisibleMetadata(item));
  if (!isRecord(value)) return value;
  const out: Record<string, unknown> = {};
  for (const [key, child] of Object.entries(value)) {
    if (MODEL_VISIBLE_ADVISORY_KEYS.has(key) || key.endsWith("_hint")) continue;
    out[key] = neutralizeModelVisibleMetadata(child, key);
  }
  return out;
}

function isSkillSurface(toolName?: string, args?: Record<string, unknown>): boolean {
  const method = toolName === "herdr_call" && typeof args?.method === "string" ? args.method : null;
  return toolName === "herdr_skill" || method?.startsWith("herdr_mcp.skill.") === true;
}

function modelVisibleStructured(
  structured: Record<string, unknown>,
  toolName?: string,
  args?: Record<string, unknown>,
): Record<string, unknown> {
  // Skill surfaces keep the policy bodies the caller explicitly requested
  // (`content`, including nested per-skill bodies); the generic advisory
  // neutralization still applies to them. The marker stays for consumers, now
  // reporting that the requested text is model-visible.
  const out = structuredObject(neutralizeModelVisibleMetadata(structured));
  if (isSkillSurface(toolName, args)) out.reference_text_exposed = true;
  return out;
}

export function callToolResult(structured: Record<string, unknown>, isError = false): Record<string, unknown> {
  const visible = modelVisibleStructured(structured);
  return {
    content: [{ type: "text", text: JSON.stringify(visible) }],
    structuredContent: visible,
    ...(isError ? { isError: true } : {}),
  };
}

/** Preserve a complete local MCP CallToolResult, including image/audio content. */
function isMcpCallToolResult(value: unknown): value is Record<string, unknown> & { content: unknown[] } {
  return isRecord(value) && Array.isArray(value.content);
}

export function normalizeSuccessfulToolResult(
  value: unknown,
  toolName: string,
  args: Record<string, unknown>,
): Record<string, unknown> {
  if (!isMcpCallToolResult(value)) {
    return callToolResult(modelVisibleStructured(structuredObject(value), toolName, args));
  }

  const result: Record<string, unknown> = { ...value };
  if (isRecord(value.structuredContent)) {
    result.structuredContent = modelVisibleStructured(value.structuredContent, toolName, args);
  }
  result.content = value.content.map((item) => {
    if (!isRecord(item) || item.type !== "text" || typeof item.text !== "string") return item;
    try {
      const parsed = JSON.parse(item.text);
      if (!isRecord(parsed)) return item;
      const visible = modelVisibleStructured(parsed, toolName, args);
      return { ...item, text: JSON.stringify(visible) };
    } catch {
      // Non-JSON text is caller-facing text the Edge cannot inspect, so it
      // passes through verbatim on every surface.
      return item;
    }
  });
  return result;
}
