/**
 * contract.ts — Relay Protocol v1 contract manifest, deterministic SHA-256
 * contract hash, and compatibility/diff helper.
 *
 * The contract is the ChatGPT-visible MCP ABI of the workstation runtime. A
 * stable `contract_hash` lets the edge answer `initialize`/`tools/list` from a
 * frozen manifest (plan §5.3) and lets the edge/workstation refuse activation
 * when a runtime branch would silently change the model-visible ABI (§5.5,
 * §8). Runtime behavior can change freely while `contract_hash` stays equal.
 *
 * ─────────────────────────────────────────────────────────────────────────────
 * CANONICAL REPRESENTATION THE HASH COVERS (documented contract)
 * ─────────────────────────────────────────────────────────────────────────────
 * The hash is computed over the bare sorted TOOL ARRAY directly — no wrapping
 * object. The recorded epoch-1 baseline reproduces exactly:
 * `computeContractHash(baseline.tools)` === `sha256:3f23083ae31b977dad21b1ec9d6919c49e1067a27f7b7eea7bdd021b54770c0d`
 * for the 17 tools of the live `0.3.23` process in `docs/_wip/.contract-epoch1-baseline.json`.
 *
 * What the hash covers (the ChatGPT-visible ABI only):
 *
 *   tools — the tool list array, normalized as follows:
 *     - every tool sorted by `name` (ascending, byte order); tool-list order
 *       is intentionally NOT semantically meaningful, so sorting makes the
 *       hash independent of catalog ordering.
 *     - each tool contributes ALL its metadata as supplied (name, description,
 *       inputSchema, annotations, execution, etc.), NOT just a fixed subset,
 *       so contract-relevant attributes affect the hash.
 *     - duplicate tool names are rejected (a catalog with two tools sharing a
 *       name is ambiguous and cannot hash deterministically).
 *     - schema-internal array order is PRESERVED: JSON-Schema arrays such as
 *       `required`, `enum`, `oneOf` and tuple `items` are semantically
 *       order-sensitive and are never permuted.
 *
 * It explicitly DOES NOT cover (so runtime/edge bookkeeping can vary without
 * perturbing the ABI):
 *   - `contract_hash` (the value itself)
 *   - `contract_epoch`
 *   - `runtime_version`
 *   - `git_commit`
 *   - any manifest metadata outside the tool catalog (captured_at, source, …)
 *
 * Canonical encoding of the covered ABI: keys sorted recursively
 * (canonical-json.ts), tools sorted by name, arrays preserved. The result is
 * the UTF-8 bytes of that canonical JSON array fed to SHA-256, reported as
 * `sha256:<lowercase hex>`.
 * ─────────────────────────────────────────────────────────────────────────────
 */

import { createHash } from "node:crypto";
import { canonicalBytes } from "./canonical-json.js";

export const CONTRACT_HASH_PREFIX = "sha256:";
/** "sha256:" + 64 hex chars. */
export const CONTRACT_HASH_LEN = 7 + 64;

/** A tool as the workstation advertises it (model-visible ABI). */
export interface ContractTool {
  name: string;
  description?: string;
  inputSchema?: Record<string, unknown>;
  /** Additional ABI metadata (annotations, execution, etc.) — hashed too. */
  [key: string]: unknown;
}

/** Full contract manifest (model-visible ABI + identity metadata). */
export interface ContractManifest {
  contract_epoch: number;
  contract_hash: string;
  runtime_version: string;
  git_commit: string | null;
  tools: ContractTool[];
  /** Optional extra manifest fields the caller attaches; they are NOT hashed. */
  [key: string]: unknown;
}

/**
 * Normalize the tool list into the hash-covered canonical form: dedupe check
 * by name, then sort by name (byte order). Array order of the *tool list* is
 * intentionally ignored; schema-internal arrays are untouched.
 */
export function normalizeTools(tools: readonly ContractTool[]): ContractTool[] {
  const seen = new Set<string>();
  const normalized = tools.map((t) => {
    if (!t || typeof t !== "object") throw new TypeError("contract: tool must be an object");
    if (typeof t.name !== "string" || t.name.length === 0) {
      throw new TypeError("contract: each tool must have a non-empty name");
    }
    if (seen.has(t.name)) throw new Error(`contract: duplicate tool name '${t.name}'`);
    seen.add(t.name);
    return { ...t };
  });
  return normalized.sort((a, b) => (a.name < b.name ? -1 : a.name > b.name ? 1 : 0));
}

/** The covered (hash-relevant) projection of a manifest: the normalized tool array itself. */
function hashProjection(tools: readonly ContractTool[]): ContractTool[] {
  return normalizeToolsForHash(tools);
}

/**
 * Build the exact canonical tool array that the hash covers. Exposed so tests and
 * tooling can inspect the deterministic input independent of hash formatting.
 */
export function contractHashSource(tools: readonly ContractTool[]): ContractTool[] {
  return hashProjection(tools);
}

function normalizeToolsForHash(tools: readonly ContractTool[]): ContractTool[] {
  return normalizeTools(tools);
}

/**
 * Compute the deterministic SHA-256 contract hash over the canonical ABI.
 * See the file header for the documented canonical representation. Output is
 * `sha256:<lowercase hex>`.
 */
export function computeContractHash(tools: readonly ContractTool[]): string {
  const source = hashProjection(tools);
  const canonical = canonicalBytes(source, 64);
  const h = createHash("sha256");
  h.update(canonical);
  return `${CONTRACT_HASH_PREFIX}${h.digest("hex")}`;
}

/**
 * Build a full manifest with a computed `contract_hash` over `tools`.
 * `runtime_version`/`git_commit`/`contract_epoch` are recorded but NOT hashed
 * (see header). Duplicate tool names throw.
 */
export function buildContractManifest(input: {
  contract_epoch: number;
  runtime_version: string;
  git_commit: string | null;
  tools: readonly ContractTool[];
}): ContractManifest {
  const tools = normalizeTools(input.tools);
  const contract_hash = computeContractHash(tools);
  return {
    contract_epoch: input.contract_epoch,
    contract_hash,
    runtime_version: input.runtime_version,
    git_commit: input.git_commit,
    tools,
  };
}

/** Verify a manifest's declared hash against its tools. */
export function verifyContractHash(manifest: ContractManifest): boolean {
  const expected = manifest.contract_hash;
  if (typeof expected !== "string" || !expected.startsWith(CONTRACT_HASH_PREFIX)) return false;
  const actual = computeContractHash(manifest.tools);
  return actual === expected;
}

/** `sha256:<hex>` syntax check (no cryptographic verification). */
export function isContractHashShape(value: unknown): value is string {
  if (typeof value !== "string") return false;
  return /^sha256:[0-9a-f]{64}$/.test(value);
}

// ─────────────────────────────────────────────────────────────────────────────
// Compatibility / diff
// ─────────────────────────────────────────────────────────────────────────────

export type ToolDiff =
  | { kind: "added"; name: string }
  | { kind: "removed"; name: string }
  | { kind: "changed"; name: string; reason: string };

export interface ContractDiff {
  equal: boolean;
  sameHash: boolean;
  sameEpoch: boolean;
  /** Added/removed/changed tools between two manifests (by name). */
  toolDiff: ToolDiff[];
}

/**
 * Compare two manifests and classify changes. The result is used to block
 * unattended runtime activation when the model-visible ABI would change.
 * `equal` is true only when epoch, hash and the tool catalog all agree.
 */
export function diffContracts(a: ContractManifest, b: ContractManifest): ContractDiff {
  const sameEpoch = a.contract_epoch === b.contract_epoch;
  const sameHash =
    typeof a.contract_hash === "string" &&
    typeof b.contract_hash === "string" &&
    a.contract_hash === b.contract_hash;
  const addedDiff: ToolDiff[] = [];
  const removedDiff: ToolDiff[] = [];
  const changedDiff: ToolDiff[] = [];
  const mapA = new Map<string, ContractTool>(a.tools.map((t) => [t.name, t]));
  const mapB = new Map<string, ContractTool>(b.tools.map((t) => [t.name, t]));
  const names = new Set([...mapA.keys(), ...mapB.keys()]);
  for (const name of names) {
    const ta = mapA.get(name);
    const tb = mapB.get(name);
    if (ta && !tb) {
      removedDiff.push({ kind: "removed", name });
    } else if (!ta && tb) {
      addedDiff.push({ kind: "added", name });
    } else if (ta && tb) {
      // Compare the hash-relevant projection (canonical tools, order-insensitive).
      const ha = computeContractHash([ta]);
      const hb = computeContractHash([tb]);
      if (ha !== hb) {
        changedDiff.push({ kind: "changed", name, reason: `canonical ABI differs (${ha} != ${hb})` });
      }
    }
  }
  const toolDiff = [...addedDiff, ...removedDiff, ...changedDiff];
  const equal = sameEpoch && sameHash && toolDiff.length === 0;
  return { equal, sameEpoch, sameHash, toolDiff };
}
