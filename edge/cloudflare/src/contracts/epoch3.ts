import { EPOCH2_CONTRACT } from "./epoch2.js";

const HERDR_DEVICES_TOOL = {
  name: "herdr_devices",
  description: "List devices registered with this Herdr Worker and their current routability/runtime status. Edge-local and read-only; never forwarded to a workstation.",
  inputSchema: {
    "$schema": "http://json-schema.org/draft-07/schema#",
    type: "object",
    properties: {},
    additionalProperties: false,
  },
  annotations: { readOnlyHint: true },
  execution: { taskSupport: "forbidden" },
} as const;

/** First device-aware ChatGPT-visible contract. Runtime execution remains epoch 2. */
export const EPOCH3_CONTRACT = {
  contract_epoch: 3,
  contract_hash: "sha256:c622546969b85e5d4c94a2f0ee1a419cd9790496b7d506de5669381354732b25",
  tool_count: 19,
  tools: [...EPOCH2_CONTRACT.tools, HERDR_DEVICES_TOOL],
} as const;
