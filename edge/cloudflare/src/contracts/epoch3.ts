import { EPOCH2_CONTRACT } from "./epoch2.js";

const DEVICE_SELECTOR_SCHEMA = {
  type: "string",
  minLength: 1,
  maxLength: 128,
  description: "Optional Herdr device_id (dev_<ULID>) or unique device name. Edge routing metadata; removed before the runtime epoch-2 call is forwarded.",
} as const;

const PUBLIC_RUNTIME_TOOLS = EPOCH2_CONTRACT.tools.map((tool) => ({
  ...tool,
  inputSchema: {
    ...tool.inputSchema,
    properties: {
      ...tool.inputSchema.properties,
      device: DEVICE_SELECTOR_SCHEMA,
    },
  },
}));

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
  contract_hash: "sha256:b8b4e5d13ccb3a1a7ab0c2e9ccfa913c076d0e1cd978cfe544d1261ea2509071",
  tool_count: 19,
  tools: [...PUBLIC_RUNTIME_TOOLS, HERDR_DEVICES_TOOL],
} as const;
