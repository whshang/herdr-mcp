/** version.ts — edge identity shared by dev/prod Worker deployments. */
import { EPOCH1_CONTRACT } from "./contracts/epoch1.js";

export const EDGE_VERSION = "0.1.0-dev";
export const EDGE_PROJECT = "herdr-edge-dev";

export const CONTRACT_EPOCH = EPOCH1_CONTRACT.contract_epoch;
export const CONTRACT_HASH = EPOCH1_CONTRACT.contract_hash;
/** Temporary compatibility alias until mcp-placeholder.ts is removed. */
export const CONTRACT_HASH_PLACEHOLDER = CONTRACT_HASH;

export interface EdgeIdentity {
  edgeProject: string;
  edgeVersion: string;
  edgeEnv: string;
  contractEpoch: number;
  contractHash: string;
}

export function edgeIdentity(options: {
  edgeEnv?: string;
  edgeVersion?: string;
  edgeProject?: string;
} = {}): EdgeIdentity {
  return {
    edgeProject: options.edgeProject ?? EDGE_PROJECT,
    edgeVersion: options.edgeVersion ?? EDGE_VERSION,
    edgeEnv: options.edgeEnv ?? "dev",
    contractEpoch: CONTRACT_EPOCH,
    contractHash: CONTRACT_HASH,
  };
}