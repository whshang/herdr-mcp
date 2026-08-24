/** version.ts — edge identity shared by dev/prod Worker deployments. */
import { PUBLIC_CONTRACT } from "./contracts/public.js";

export const EDGE_VERSION = "0.1.0-dev";
export const EDGE_PROJECT = "herdr-edge-dev";
export const MCP_SERVER_VERSION = "0.3.32";

export const CONTRACT_EPOCH = PUBLIC_CONTRACT.contract_epoch;
export const CONTRACT_HASH = PUBLIC_CONTRACT.contract_hash;

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