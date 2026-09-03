import assert from "node:assert/strict";
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import test from "node:test";
import { fileURLToPath } from "node:url";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const SCRIPT = path.join(ROOT, "scripts", "sign-relay-manifest.mjs");
const DOMAIN = Buffer.from("HERDR-RELAY-POOL-V1\0", "utf8");

function runSigner(args) {
  return spawnSync(process.execPath, [SCRIPT, ...args], {
    cwd: ROOT,
    encoding: "utf8",
  });
}

function fixture() {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "herdr-relay-signer-"));
  const { privateKey, publicKey } = crypto.generateKeyPairSync("ed25519");
  const key = path.join(dir, "private.pem");
  fs.writeFileSync(key, privateKey.export({ format: "pem", type: "pkcs8" }), { mode: 0o600 });
  return { dir, key, publicKey };
}

test("relay manifest signer signs exact payload bytes with the frozen domain", () => {
  const { dir, key, publicKey } = fixture();
  try {
    const payload = path.join(dir, "payload.json");
    const output = path.join(dir, "envelope.json");
    const payloadBytes = Buffer.from(
      JSON.stringify({
        schema: 1,
        revision: 7,
        generated_at: "2026-09-03T06:00:00Z",
        not_before: "2026-09-03T06:00:00Z",
        expires_at: "2026-09-10T06:00:00Z",
        relays: [
          {
            id: "deno-prod",
            url: "wss://relay.herdr-mcp.deno.net/v1",
            priority: 200,
            weight: 100,
            failure_domain: "deno",
            enabled: true,
          },
        ],
      }),
      "utf8",
    );
    fs.writeFileSync(payload, payloadBytes);

    const result = runSigner([
      "--private-key",
      key,
      "--key-id",
      "relay-test-key",
      "--payload",
      payload,
      "--output",
      output,
    ]);
    assert.equal(result.status, 0, result.stderr);

    const summary = JSON.parse(result.stdout.trim());
    assert.equal(summary.ok, true);
    assert.equal(summary.key_id, "relay-test-key");
    assert.equal(summary.revision, 7);
    assert.equal(summary.public_key_base64url, publicKey.export({ format: "jwk" }).x);

    const envelope = JSON.parse(fs.readFileSync(output, "utf8"));
    assert.deepEqual(Object.keys(envelope).sort(), ["key_id", "payload", "signature"]);
    assert.deepEqual(Buffer.from(envelope.payload, "base64"), payloadBytes);

    const length = Buffer.alloc(4);
    length.writeUInt32BE(payloadBytes.length, 0);
    const message = Buffer.concat([DOMAIN, length, payloadBytes]);
    assert.equal(
      crypto.verify(null, message, publicKey, Buffer.from(envelope.signature, "base64")),
      true,
    );
    assert.equal(result.stdout.includes("BEGIN PRIVATE KEY"), false);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test("relay manifest signer rejects unsafe relay targets before signing", () => {
  const { dir, key } = fixture();
  try {
    const payload = path.join(dir, "payload.json");
    const output = path.join(dir, "envelope.json");
    fs.writeFileSync(
      payload,
      JSON.stringify({
        schema: 1,
        revision: 1,
        generated_at: "2026-09-03T06:00:00Z",
        not_before: "2026-09-03T06:00:00Z",
        expires_at: "2026-09-10T06:00:00Z",
        relays: [
          {
            id: "bad",
            url: "wss://127.0.0.1/v1",
            priority: 100,
            failure_domain: "bad",
            enabled: true,
          },
        ],
      }),
    );

    const result = runSigner([
      "--private-key",
      key,
      "--key-id",
      "relay-test-key",
      "--payload",
      payload,
      "--output",
      output,
    ]);
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /must not use an IP literal/);
    assert.equal(fs.existsSync(output), false);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});
