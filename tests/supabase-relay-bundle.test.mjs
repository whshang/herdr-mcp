import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { expectedSupabaseRelaySource } from "../scripts/build-supabase-relay.mjs";

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const generatedPath = path.join(
  repoRoot,
  "supabase/functions/herdr-relay/index.ts",
);

test("Supabase Relay bundle is generated from the Deno Relay source", () => {
  const actual = fs.readFileSync(generatedPath, "utf8");
  const expected = expectedSupabaseRelaySource();
  assert.equal(actual, expected);
  assert.match(actual, /pathPrefix: "\/herdr-relay"/);
  assert.match(actual, /waitUntil: edgeRuntime\?\.waitUntil\.bind\(edgeRuntime\)/);
  assert.doesNotMatch(actual, /Entrypoint for `deno run` or Deno Deploy/);
});
