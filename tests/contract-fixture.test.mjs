import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const root = new URL("../", import.meta.url);

async function readSourceContract() {
  const source = await readFile(new URL("edge/cloudflare/src/contracts/epoch2.ts", root), "utf8");
  const jsonText = source
    .replace(/^\/\*\*[^\n]*\*\/\n/, "")
    .replace(/^export const EPOCH2_CONTRACT = /, "")
    .replace(/ as const;\s*$/, "");
  return JSON.parse(jsonText);
}

test("epoch2 JSON fixture is the language-independent public contract source", async () => {
  const fixture = JSON.parse(await readFile(new URL("contracts/epoch2.json", root), "utf8"));
  const sourceContract = await readSourceContract();

  assert.deepEqual(sourceContract, fixture);
  assert.equal(fixture.contract_epoch, 2);
  assert.equal(fixture.tool_count, 18);
  assert.equal(fixture.tools.length, 18);
  assert.equal(new Set(fixture.tools.map((tool) => tool.name)).size, 18);
  assert.equal(fixture.tools.some((tool) => tool.name.startsWith("herdr_mcp.")), false);
});

test("runtime parity fixture pins the shared Node/Rust wire invariants", async () => {
  const parity = JSON.parse(await readFile(new URL("contracts/runtime-parity.json", root), "utf8"));
  const contract = JSON.parse(await readFile(new URL("contracts/runtime-exec-v5.json", root), "utf8"));
  assert.equal(parity.schema_version, 1);
  assert.equal(parity.server_name, "herdr-mcp");
  assert.equal(parity.sdk_wire_protocol, "2025-11-25");
  assert.deepEqual(parity.supported_versions.slice(0, 2), ["2025-11-25", "2025-06-18"]);
  assert.equal(parity.contract_epoch, contract.contract_epoch);
  assert.equal(parity.contract_hash, contract.contract_hash);
  assert.equal(parity.tool_count, contract.tool_count);
  assert.deepEqual(parity.stateless_handshake_sse_methods, ["initialize", "tools/list"]);
  assert.deepEqual(parity.stateless_json_methods, ["server/discover", "tools/call"]);
});

test("user keeps relay runtime identity aligned | Given runtime-exec-v5 is current | When relay adapter constants are read | Then epoch and hash match the descriptor", async () => {
  const descriptor = JSON.parse(
    await readFile(new URL("contracts/runtime-exec-v5.json", root), "utf8"),
  );

  for (const relativePath of [
    "relay/deno/server.ts",
    "relay/supabase/functions/herdr-relay/index.ts",
  ]) {
    const source = await readFile(new URL(relativePath, root), "utf8");
    const epochMatch = source.match(/export const EXPECTED_RUNTIME_CONTRACT_EPOCH = (\d+);/);
    const hashMatch = source.match(/export const EXPECTED_RUNTIME_CONTRACT_HASH =\s*"([^"]+)";/);

    assert.ok(epochMatch, `EXPECTED_RUNTIME_CONTRACT_EPOCH constant found in ${relativePath}`);
    assert.ok(hashMatch, `EXPECTED_RUNTIME_CONTRACT_HASH constant found in ${relativePath}`);
    assert.equal(Number(epochMatch[1]), descriptor.contract_epoch, `epoch in ${relativePath} matches runtime-exec-v5 descriptor`);
    assert.equal(hashMatch[1], descriptor.contract_hash, `hash in ${relativePath} matches runtime-exec-v5 descriptor`);
  }
});

test("user keeps Edge runtime identity aligned | Given frozen epoch-2 and runtime-exec-v5 descriptors | When Edge compatibility identities are read | Then current and rollback identities match", async () => {
  const descriptor = JSON.parse(
    await readFile(new URL("contracts/runtime-exec-v5.json", root), "utf8"),
  );
  const source = await readFile(
    new URL("edge/cloudflare/src/contracts/runtime.ts", root),
    "utf8",
  );
  // Anchor on the current identity export: the frozen predecessor identity is
  // declared above it in the same module.
  const current = source.match(
    /export const RUNTIME_EXECUTION_CONTRACT = \{([\s\S]*?)\} as const;/,
  );
  assert.ok(current, "current runtime execution identity export present");
  const epochMatch = current[1].match(/contract_epoch: (\d+),/);
  const hashMatch = current[1].match(/contract_hash: "([^"]+)"/);
  const countMatch = current[1].match(/tool_count: (\d+),/);
  assert.ok(epochMatch && hashMatch && countMatch, "edge runtime identity fields present");
  assert.equal(Number(epochMatch[1]), descriptor.contract_epoch);
  assert.equal(hashMatch[1], descriptor.contract_hash);
  assert.equal(Number(countMatch[1]), descriptor.tool_count);
  // The acceptance window is the current identity plus the previous
  // runtime execution identity and the frozen epoch-2 catalog.
  assert.match(
    source,
    /COMPATIBLE_RUNTIME_CONTRACTS = \[\s*RUNTIME_EXECUTION_CONTRACT,\s*PREVIOUS_RUNTIME_EXECUTION_CONTRACT,\s*EPOCH2_CONTRACT,\s*\]/,
  );
  assert.match(source, /contract_epoch: 4,/);
  assert.match(
    source,
    /PREVIOUS_RUNTIME_EXECUTION_CONTRACT = \{\s*contract_epoch: 4,\s*contract_hash: "sha256:1f4d272cedb3334b3e17e08080793f6ed81a03dccffba2f6434f149b10e2e135",\s*tool_count: 18,\s*\}/,
  );
});
