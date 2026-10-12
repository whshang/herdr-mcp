// JS <-> Rust acceptance-digest parity contract.
//
// The browser extension reports the digests of the accepted provider text and
// the Rust runtime re-verifies them. If the two languages canonicalize text
// differently the result is a silent "uncertain" that never reconciles, so both
// sides are pinned to one shared fixture and each side hashes it with its own
// shipped helper:
//   - this test uses the real extension helpers sliced out of
//     extension/content/wake.js
//   - crates/herdr-mcp/src/mcp.rs has the matching Rust test
//     (browser_acceptance_digest_parity_with_extension_fixture)
// JS `String.prototype.trim()` / `\s` must not be used here: they differ from
// Rust on U+FEFF (BOM) and U+0085 (NEL).
import test from "node:test";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";

const wakeSource = readFileSync(new URL("../extension/content/wake.js", import.meta.url), "utf8");
const fixture = JSON.parse(
  readFileSync(new URL("./fixtures/browser-acceptance-digest-parity.json", import.meta.url), "utf8"),
);

const start = wakeSource.indexOf("  // The exact Unicode White_Space set Rust");
const end = wakeSource.indexOf("  // Strip only leading `@?<app>` mention tokens", start);
assert.ok(
  start >= 0 && end > start,
  "acceptance trim + canonical helpers must remain extractable from wake.js",
);
const { acceptanceTrim, canonicalAcceptanceText } = new Function(
  `${wakeSource.slice(start, end)}
   return { acceptanceTrim, canonicalAcceptanceText };`,
)();

const sha256 = (text) => createHash("sha256").update(text, "utf8").digest("hex");

test("user gets identical acceptance digests in the extension and the runtime | Given shared whitespace fixtures | When both sides hash the accepted text | Then trimmed and canonical digests match", () => {
  assert.equal(fixture.schema_version, 1);
  const cases = fixture.cases;
  assert.ok(Array.isArray(cases) && cases.length >= 10, "fixture must pin a real whitespace matrix");
  for (const entry of cases) {
    assert.equal(
      sha256(acceptanceTrim(entry.text)),
      entry.trimmed_sha256,
      `${entry.name}: acceptance trim digest must match the shared contract`,
    );
    assert.equal(
      sha256(canonicalAcceptanceText(entry.text)),
      entry.canonical_sha256,
      `${entry.name}: canonical digest must match the shared contract`,
    );
  }
});

test("user never gets a JS-only trim accepted as the provider digest | Given a BOM or NEL boundary | When the extension trims for the runtime | Then it follows Rust Unicode White_Space instead of String.prototype.trim", () => {
  const bomLeading = "﻿one two";
  const nelLeading = "\u0085one two";
  assert.equal(
    acceptanceTrim(bomLeading),
    bomLeading,
    "Rust str::trim() does not strip U+FEFF, so the extension must not either",
  );
  assert.notEqual(acceptanceTrim(bomLeading), bomLeading.trim());
  assert.equal(acceptanceTrim(nelLeading), "one two", "U+0085 is Rust White_Space");
  assert.notEqual(
    acceptanceTrim(nelLeading),
    nelLeading.trim(),
    "String.prototype.trim() does not strip U+0085, so it must not be used",
  );
  assert.equal(
    canonicalAcceptanceText("one\u0085two"),
    "one two",
    "U+0085 must collapse like every other Unicode White_Space run",
  );
  assert.equal(canonicalAcceptanceText("one\u00a0two"), "one two");
  assert.equal(canonicalAcceptanceText("one﻿two"), "one two");
});
