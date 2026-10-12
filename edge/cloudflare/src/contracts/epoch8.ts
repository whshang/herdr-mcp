import { EPOCH7_CONTRACT } from "./epoch7.js";

/**
 * Public epoch 8 clarifies generic fallback surfaces without changing
 * the 19 tools, parameter schemas, or safety annotations in epoch 7.
 */
const FALLBACK_DESCRIPTIONS: Record<string, string> = {
  herdr_call:
    "Use this for supported Herdr, Skill, and browser actions without a dedicated public tool. Browser actions require an explicit device. Inputs are validated; effects depend on the selected action.",
  herdr_exec:
    "Use this to run project commands, including shell scripts, pipes, or redirects, and bounded executable steps when dedicated file or Git tools are insufficient. A timeout can return a session ID for the same continuing run.",
  herdr_skill:
    "Use this to read Herdr operating guidance for choosing dedicated tools, generic actions, and safe mutation recovery. Returns runtime context and supports refreshing configured reference material.",
};

export const EPOCH8_CONTRACT = {
  contract_epoch: 8,
  contract_hash: "sha256:16c84060cb0f81725643fb0a1fc5cf08197839276cace8f8bbc642c108aa1311",
  tool_count: 19,
  tools: EPOCH7_CONTRACT.tools.map((tool) => ({
    ...tool,
    description: FALLBACK_DESCRIPTIONS[tool.name] ?? tool.description,
  })),
} as const;
