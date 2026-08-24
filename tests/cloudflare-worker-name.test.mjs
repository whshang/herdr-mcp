import test from "node:test";
import assert from "node:assert/strict";
import {
  cloudflareMachineSlug,
  cloudflareWorkerName,
  CLOUDFLARE_WORKER_NAME_MAX,
} from "../scripts/cloudflare-worker-name.mjs";

test("Cloudflare Worker names use a deterministic Cloudflare-safe machine slug", () => {
  assert.equal(cloudflareWorkerName("MacBook.local"), "herdr-edge-macbook-local");
  assert.equal(cloudflareWorkerName("wh.shang"), "herdr-edge-wh-shang");
  assert.equal(cloudflareWorkerName("alpha..beta___gamma"), "herdr-edge-alpha-beta-gamma");

  for (const input of [
    "中文主机名",
    "...___...",
    "a".repeat(200),
    "Office Mac (M4).local",
  ]) {
    const name = cloudflareWorkerName(input);
    assert.match(name, /^[a-z0-9](?:[a-z0-9-]*[a-z0-9])?$/);
    assert.ok(name.length <= CLOUDFLARE_WORKER_NAME_MAX);
    assert.doesNotMatch(name, /[._]/);
    assert.equal(name, cloudflareWorkerName(input));
  }
});

test("non-ASCII or all-special slugs use deterministic hash fallback", () => {
  assert.match(cloudflareMachineSlug("中文主机名"), /^host-[0-9a-f]{10}$/);
  assert.match(cloudflareMachineSlug("...___..."), /^host-[0-9a-f]{10}$/);
});
