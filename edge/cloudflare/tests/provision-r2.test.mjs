import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import {
  isAlreadyExistsError,
  parseR2Bindings,
  provisionR2Buckets,
} from "../provision-r2.mjs";

const DIR = path.dirname(fileURLToPath(import.meta.url));

test("provisioner parses private ARTIFACT_BUCKET bindings from wrangler configs", () => {
  const dev = parseR2Bindings(readFileSync(path.join(DIR, "../wrangler.toml"), "utf8"));
  const prod = parseR2Bindings(readFileSync(path.join(DIR, "../wrangler.prod.toml"), "utf8"));
  const user = parseR2Bindings(readFileSync(path.join(DIR, "../wrangler.user.example.toml"), "utf8"));
  assert.deepEqual(dev, [{ binding: "ARTIFACT_BUCKET", bucket_name: "herdr-edge-dev-artifacts" }]);
  assert.deepEqual(prod, [{ binding: "ARTIFACT_BUCKET", bucket_name: "herdr-edge-prod-artifacts" }]);
  assert.deepEqual(user, []);
});

test("provisioner treats already-exists as success and can no-op deploy", async () => {
  assert.equal(isAlreadyExistsError("A bucket with the name herdr-edge-prod-artifacts already exists."), true);
  const calls = [];
  const result = await provisionR2Buckets({
    configPath: path.join(DIR, "../wrangler.prod.toml"),
    wrangler: "npx --yes wrangler@4",
    deploy: true,
    run: async (bin, args) => {
      calls.push([bin, ...args]);
      if (args.includes("create")) {
        return { code: 1, stdout: "", stderr: "The bucket you are trying to create already exists" };
      }
      return { code: 0, stdout: "deployed", stderr: "" };
    },
  });
  assert.equal(result.ok, true);
  assert.deepEqual(result.existing, ["herdr-edge-prod-artifacts"]);
  assert.equal(result.deployed, true);
  assert.equal(calls[0].includes("r2"), true);
  assert.equal(calls[0].includes("create"), true);
  assert.equal(calls[1].includes("deploy"), true);
});

test("provisioner dry-run does not invoke Cloudflare", async () => {
  let called = false;
  const result = await provisionR2Buckets({
    configPath: path.join(DIR, "../wrangler.toml"),
    dryRun: true,
    run: async () => {
      called = true;
      return { code: 1, stdout: "", stderr: "should not run" };
    },
  });
  assert.equal(result.dryRun, true);
  assert.equal(called, false);
  assert.equal(result.buckets[0].bucket_name, "herdr-edge-dev-artifacts");
});
