import { EPOCH3_CONTRACT } from "./epoch3.js";
import { EPOCH7_CONTRACT } from "./epoch7.js";

/** Conservative fallback ChatGPT-visible device-aware public contract. */
export const PUBLIC_CONTRACT = EPOCH3_CONTRACT;

export function resolvePublicContract(edgeEnv?: string) {
  return edgeEnv === "dev" || edgeEnv === "prod" ? EPOCH7_CONTRACT : PUBLIC_CONTRACT;
}
