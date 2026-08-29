import { test } from "node:test";
import assert from "node:assert/strict";
import {
  checkFrameSize,
  parseJsonFrame,
  checkArgsBudget,
  readBodyBounded,
  readBytesBounded,
  byteLengthOf,
} from "../dist/payload.js";

const MB = 1_048_576;

test("payload: byteLength matches utf8 multibyte", () => {
  assert.equal(byteLengthOf("héllo"), 6);
  assert.equal(byteLengthOf("日本語"), 9);
});

test("payload: oversized frame rejected before parse", () => {
  const big = "x".repeat(MB + 10);
  const r = checkFrameSize(big, MB);
  assert.equal(r.ok, false);
  assert.equal(r.code, "payload_too_large");
  assert.equal(r.bytes, MB + 10);
});

test("payload: parseJsonFrame rejects non-JSON", () => {
  const r = parseJsonFrame("not json", MB);
  assert.equal(r.ok, false);
  assert.equal(r.code, "bad_request");
});

test("payload: parseJsonFrame parses bounded object", () => {
  const r = parseJsonFrame(JSON.stringify({ a: 1 }), MB);
  assert.equal(r.ok, true);
  assert.deepEqual(r.value, { a: 1 });
});

test("payload: readBodyBounded rejects declared content-length over budget", async () => {
  const body = {
    headers: { get: (n) => (n === "content-length" ? String(MB + 5) : null) },
    text: async () => "x".repeat(MB + 5),
  };
  const r = await readBodyBounded(body, MB);
  assert.equal(r.ok, false);
  assert.equal(r.code, "payload_too_large");
});

test("payload: readBodyBounded accepts small body", async () => {
  const body = {
    headers: { get: () => null },
    text: async () => JSON.stringify({ ok: true }),
  };
  const r = await readBodyBounded(body, MB);
  assert.equal(r.ok, true);
  assert.deepEqual(r.value, { ok: true });
});

test("payload: readBytesBounded rejects declared content-length over budget", async () => {
  const body = {
    headers: { get: (n) => (n === "content-length" ? String(MB + 5) : null) },
    arrayBuffer: async () => new Uint8Array(1).buffer,
  };
  const r = await readBytesBounded(body, MB);
  assert.equal(r.ok, false);
  assert.equal(r.code, "payload_too_large");
});

test("payload: readBytesBounded accepts raw image bytes", async () => {
  const bytes = Uint8Array.from([1, 2, 3, 4]);
  const r = await readBytesBounded({
    headers: { get: () => null },
    arrayBuffer: async () => bytes.buffer,
  }, MB);
  assert.equal(r.ok, true);
  assert.deepEqual([...r.bytes], [1, 2, 3, 4]);
});

test("payload: checkArgsBudget rejects oversized args", () => {
  const r = checkArgsBudget({ data: "x".repeat(MB + 1) }, MB);
  assert.equal(r.ok, false);
  assert.equal(r.code, "payload_too_large");
});