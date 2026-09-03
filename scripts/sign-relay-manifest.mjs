#!/usr/bin/env node

import crypto from "node:crypto";
import fs from "node:fs";
import net from "node:net";
import path from "node:path";

const SIGNING_DOMAIN = Buffer.from("HERDR-RELAY-POOL-V1\0", "utf8");
const MAX_PAYLOAD_BYTES = 128 * 1024;
const MAX_RELAYS = 32;
const MAX_KEY_ID_LEN = 64;
const MAX_RELAY_ID_LEN = 64;
const MAX_FAILURE_DOMAIN_LEN = 128;
const MAX_RELAY_URL_LEN = 2048;
const MAX_PRIORITY = 1_000_000;

function fail(message) {
  console.error(`relay-manifest-sign: ${message}`);
  process.exit(1);
}

function usage() {
  console.error(
    "usage: node scripts/sign-relay-manifest.mjs --private-key <pkcs8.pem> --key-id <id> --payload <payload.json> --output <envelope.json>",
  );
  process.exit(2);
}

function parseArgs(argv) {
  const args = new Map();
  for (let i = 0; i < argv.length; i += 2) {
    const key = argv[i];
    const value = argv[i + 1];
    if (!key?.startsWith("--") || value == null || value.startsWith("--")) usage();
    if (args.has(key)) fail(`duplicate argument ${key}`);
    args.set(key, value);
  }
  for (const required of ["--private-key", "--key-id", "--payload", "--output"]) {
    if (!args.has(required)) usage();
  }
  return args;
}

function exactKeys(value, expected, label) {
  const got = Object.keys(value).sort();
  const want = [...expected].sort();
  if (got.length !== want.length || got.some((key, index) => key !== want[index])) {
    fail(`${label} has unexpected fields`);
  }
}

function boundedString(value, max, label) {
  if (typeof value !== "string" || value.length === 0 || value.length > max) {
    fail(`${label} must be a non-empty bounded string`);
  }
}

function parseTimestamp(value, label) {
  boundedString(value, 64, label);
  const parsed = Date.parse(value);
  if (!Number.isFinite(parsed)) fail(`${label} must be RFC3339-compatible`);
  return parsed;
}

function validateRelay(relay, index) {
  if (!relay || typeof relay !== "object" || Array.isArray(relay)) fail(`relay[${index}] must be an object`);
  exactKeys(relay, ["id", "url", "priority", "failure_domain", "enabled"], `relay[${index}]`);
  boundedString(relay.id, MAX_RELAY_ID_LEN, `relay[${index}].id`);
  boundedString(relay.failure_domain, MAX_FAILURE_DOMAIN_LEN, `relay[${index}].failure_domain`);
  boundedString(relay.url, MAX_RELAY_URL_LEN, `relay[${index}].url`);
  if (!Number.isInteger(relay.priority) || relay.priority < 0 || relay.priority > MAX_PRIORITY) {
    fail(`relay[${index}].priority is out of range`);
  }
  if (typeof relay.enabled !== "boolean") fail(`relay[${index}].enabled must be boolean`);

  let url;
  try {
    url = new URL(relay.url);
  } catch {
    fail(`relay[${index}].url is invalid`);
  }
  if (!['wss:', 'https:'].includes(url.protocol)) fail(`relay[${index}].url must use WSS/HTTPS`);
  if (!url.hostname || url.username || url.password || url.search || url.hash || url.port) {
    fail(`relay[${index}].url must not contain credentials, query, fragment, or explicit port`);
  }
  if (net.isIP(url.hostname) !== 0) fail(`relay[${index}].url must not use an IP literal`);
}

function validatePayload(payload) {
  if (!payload || typeof payload !== "object" || Array.isArray(payload)) fail("payload must be an object");
  exactKeys(payload, ["schema", "revision", "generated_at", "not_before", "expires_at", "relays"], "payload");
  if (payload.schema !== 1) fail("payload.schema must be 1");
  if (!Number.isSafeInteger(payload.revision) || payload.revision <= 0) fail("payload.revision must be a positive integer");
  if (!Array.isArray(payload.relays) || payload.relays.length > MAX_RELAYS) fail("payload.relays is invalid or too large");
  const generatedAt = parseTimestamp(payload.generated_at, "payload.generated_at");
  const notBefore = parseTimestamp(payload.not_before, "payload.not_before");
  const expiresAt = parseTimestamp(payload.expires_at, "payload.expires_at");
  if (generatedAt > expiresAt || notBefore > expiresAt) fail("payload timestamp ordering is invalid");
  const ids = new Set();
  for (const [index, relay] of payload.relays.entries()) {
    validateRelay(relay, index);
    if (ids.has(relay.id)) fail(`duplicate relay id ${relay.id}`);
    ids.add(relay.id);
  }
}

const args = parseArgs(process.argv.slice(2));
const privateKeyPath = args.get("--private-key");
const keyId = args.get("--key-id");
const payloadPath = args.get("--payload");
const outputPath = args.get("--output");

boundedString(keyId, MAX_KEY_ID_LEN, "key id");
if (!/^[A-Za-z0-9._-]+$/.test(keyId)) fail("key id contains unsupported characters");

const payloadBytes = fs.readFileSync(payloadPath);
if (payloadBytes.length === 0 || payloadBytes.length > MAX_PAYLOAD_BYTES) fail("payload exceeds size limit");
let payload;
try {
  payload = JSON.parse(payloadBytes.toString("utf8"));
} catch {
  fail("payload is not valid UTF-8 JSON");
}
validatePayload(payload);

const privateKey = crypto.createPrivateKey(fs.readFileSync(privateKeyPath));
if (privateKey.asymmetricKeyType !== "ed25519") fail("private key must be Ed25519 PKCS#8 PEM");
const publicKey = crypto.createPublicKey(privateKey);
const publicJwk = publicKey.export({ format: "jwk" });
const publicRaw = Buffer.from(publicJwk.x, "base64url");
if (publicRaw.length !== 32) fail("unexpected Ed25519 public key length");

const length = Buffer.alloc(4);
length.writeUInt32BE(payloadBytes.length, 0);
const message = Buffer.concat([SIGNING_DOMAIN, length, payloadBytes]);
const signature = crypto.sign(null, message, privateKey);
const envelopeBytes = Buffer.from(
  `${JSON.stringify({
    key_id: keyId,
    signature: signature.toString("base64"),
    payload: payloadBytes.toString("base64"),
  })}\n`,
  "utf8",
);

const outputDir = path.dirname(outputPath);
fs.mkdirSync(outputDir, { recursive: true, mode: 0o755 });
const temporary = path.join(outputDir, `.${path.basename(outputPath)}.tmp-${process.pid}`);
try {
  fs.writeFileSync(temporary, envelopeBytes, { flag: "wx", mode: 0o644 });
  fs.renameSync(temporary, outputPath);
} finally {
  try {
    fs.unlinkSync(temporary);
  } catch (error) {
    if (error.code !== "ENOENT") throw error;
  }
}

console.log(
  JSON.stringify({
    ok: true,
    key_id: keyId,
    revision: payload.revision,
    public_key_base64url: publicJwk.x,
    payload_sha256: crypto.createHash("sha256").update(payloadBytes).digest("hex"),
    envelope_sha256: crypto.createHash("sha256").update(envelopeBytes).digest("hex"),
    output: outputPath,
  }),
);
