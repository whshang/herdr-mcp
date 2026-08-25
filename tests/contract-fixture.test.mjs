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
});
