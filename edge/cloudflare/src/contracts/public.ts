import { EPOCH3_CONTRACT } from "./epoch3.js";
import { EPOCH6_CONTRACT } from "./epoch6.js";

/** Conservative fallback ChatGPT-visible device-aware public contract. */
export const PUBLIC_CONTRACT = EPOCH3_CONTRACT;

export function resolvePublicContract(edgeEnv?: string) {
  return edgeEnv === "dev" || edgeEnv === "prod" ? EPOCH6_CONTRACT : PUBLIC_CONTRACT;
}
