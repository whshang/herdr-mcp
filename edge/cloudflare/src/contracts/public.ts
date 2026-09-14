import { EPOCH3_CONTRACT } from "./epoch3.js";
import { EPOCH4_CONTRACT } from "./epoch4.js";

/** Conservative fallback ChatGPT-visible device-aware public contract. */
export const PUBLIC_CONTRACT = EPOCH3_CONTRACT;

export function resolvePublicContract(edgeEnv?: string) {
  return edgeEnv === "dev" || edgeEnv === "prod" ? EPOCH4_CONTRACT : PUBLIC_CONTRACT;
}
