import { EPOCH3_CONTRACT } from "./epoch3.js";
import { EPOCH8_CONTRACT } from "./epoch8.js";

/** Conservative fallback ChatGPT-visible device-aware public contract. */
export const PUBLIC_CONTRACT = EPOCH3_CONTRACT;

export function resolvePublicContract(edgeEnv?: string) {
  return edgeEnv === "dev" || edgeEnv === "prod" ? EPOCH8_CONTRACT : PUBLIC_CONTRACT;
}
