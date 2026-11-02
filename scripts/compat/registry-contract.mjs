// Source-level reader for the trusted-Extension registry contract (#586).
//
// The Extension <-> Runtime compatibility surface is the *allow-list* inside
// `crates/herdr-mcp/src/mcp_http.rs` (`browser_registry_allow_fields`) plus the
// capability-operation vocabulary in `crates/herdr-mcp/src/state_store.rs`
// (`ALLOWED_BROWSER_CAPABILITY_OPERATIONS`). Both are plain Rust source, so a
// previous release plane's contract can be read straight out of its git tag
// instead of being hand-copied into a fixture that silently drifts.
import { execFileSync } from "node:child_process";

export function gitShow(repoRoot, rev, path) {
  try {
    return execFileSync("git", ["-C", repoRoot, "show", `${rev}:${path}`], {
      encoding: "utf8",
      stdio: ["ignore", "pipe", "ignore"],
      maxBuffer: 64 * 1024 * 1024,
    });
  } catch {
    return null;
  }
}

/** Resolve a tag/branch to its full 40-character commit SHA, or null. */
export function resolveCommit(repoRoot, rev) {
  try {
    return execFileSync("git", ["-C", repoRoot, "rev-parse", `${rev}^{commit}`], {
      encoding: "utf8",
      stdio: ["ignore", "pipe", "ignore"],
    }).trim();
  } catch {
    return null;
  }
}

function stringLiterals(block) {
  return [...block.matchAll(/"([^"\\]*)"/g)].map((match) => match[1]);
}

/**
 * Map `endpoint.register` / `source_turn.observe` / ... to the exact field set
 * that revision admits on `/extension/browser/registry`.
 */
export function registryAllowLists(mcpHttpSource) {
  const lists = new Map();
  const starts = [...mcpHttpSource.matchAll(/^fn extension_browser_([a-z0-9_]+)\($/gm)];
  for (const [index, match] of starts.entries()) {
    const from = match.index;
    const to = starts[index + 1]?.index ?? mcpHttpSource.length;
    const body = mcpHttpSource.slice(from, to);
    const allow = body.match(/browser_registry_allow_fields\(\s*\n?\s*payload,\s*&\[([\s\S]*?)\]/);
    if (!allow) continue;
    const operation = match[1].replace(/_([a-z0-9]+)$/, ".$1");
    lists.set(operation, stringLiterals(allow[1]));
  }
  return lists;
}

/** The capability vocabulary a Runtime accepts inside `provider.observe`. */
export function capabilityOperations(stateStoreSource) {
  const block = stateStoreSource.match(
    /const ALLOWED_BROWSER_CAPABILITY_OPERATIONS: &\[&str\] = &\[([\s\S]*?)\];/,
  );
  return block ? stringLiterals(block[1]) : null;
}

/** `SCHEMA_VERSION` (durable state schema) declared by one revision. */
export function stateSchemaVersion(stateStoreSource) {
  const match = stateStoreSource.match(/pub const SCHEMA_VERSION: i64 = (\d+);/);
  return match ? Number(match[1]) : null;
}

/** Number of registered state migrations (`SCHEMA_VERSION` must equal it). */
export function stateMigrationCount(schemaSource) {
  const block = schemaSource.match(/pub\(super\) const MIGRATIONS: &\[&str\] = &\[([\s\S]*?)\];/);
  if (!block) return null;
  return [...block[1].matchAll(/MIGRATION_V\d+/g)].length;
}
