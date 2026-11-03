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
const end = wakeSource.indexOf("  // Exact submit acceptance.", start);
assert.ok(
  start >= 0 && end > start,
  "acceptance trim + canonical + App-mention helpers must stay extractable from wake.js",
);
const { acceptanceTrim, canonicalAcceptanceText, stripLeadingAppMentionTokens } = new Function(
  `${wakeSource.slice(start, end)}
   return { acceptanceTrim, canonicalAcceptanceText, stripLeadingAppMentionTokens };`,
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

// The App pill half of the contract: a requested App is stored inside the
// accepted user turn as an inline chip whose rendering differs by provider
// build, and the runtime accepts exactly that rendering as an explicit
// expectation (#848 / #754). One fixture drives both languages, so a pill form
// stripped by the extension can never be rejected by the runtime.
test("user gets a confirmed create when the App pill is normalized | Given the shared App-mention fixtures | When the extension strips the requested App token and the runtime rebuilds its expectation | Then every tolerated pill rendering reduces to the message and every other turn still mismatches", () => {
  const cases = fixture.app_mention_cases;
  assert.ok(Array.isArray(cases) && cases.length >= 10, "fixture must pin the observed pill matrix");
  for (const entry of cases) {
    assert.equal(
      stripLeadingAppMentionTokens(entry.observed_text, [entry.app]),
      entry.stripped_body,
      `${entry.name}: the extension must strip exactly this App mention and nothing else`,
    );
    assert.equal(
      sha256(acceptanceTrim(entry.observed_text)),
      entry.observed_text_trimmed_sha256,
      `${entry.name}: the reported trimmed digest must stay pinned`,
    );
    assert.equal(
      sha256(canonicalAcceptanceText(entry.observed_text)),
      entry.observed_text_canonical_sha256,
      `${entry.name}: the reported canonical digest must stay pinned`,
    );
    if (entry.tolerated) {
      assert.equal(
        canonicalAcceptanceText(entry.stripped_body),
        canonicalAcceptanceText(entry.message),
        `${entry.name}: a tolerated pill rendering must reduce to the submitted message`,
      );
    } else {
      assert.notEqual(
        canonicalAcceptanceText(entry.stripped_body),
        canonicalAcceptanceText(entry.message),
        `${entry.name}: a different turn must never reduce to the submitted message`,
      );
    }
  }
});

test("user never gets a foreign turn accepted as the requested message | Given a body that only starts like the App pill | When the extension strips the requested App token | Then the remainder must still match the whole message", () => {
  assert.equal(stripLeadingAppMentionTokens("@herdr-mcp", ["herdr-mcp"]), "");
  assert.equal(stripLeadingAppMentionTokens("@herdr-mcp!", ["herdr-mcp"]), "@herdr-mcp!");
  assert.equal(stripLeadingAppMentionTokens("hello @herdr-mcp", ["herdr-mcp"]), "hello @herdr-mcp");
  assert.equal(
    stripLeadingAppMentionTokens("@herdr-mcp @herdr-mcp hello", ["herdr-mcp"]),
    "hello",
    "repeated pills are stripped only at the start",
  );
  assert.equal(
    stripLeadingAppMentionTokens("@herdr-mcp hello", ["herdr", "herdr-mcp"]),
    "hello",
    "the longest matching App name wins, so a shorter prefix never eats part of the pill",
  );
  assert.equal(
    stripLeadingAppMentionTokens("@herdr-mcp hello", []),
    "@herdr-mcp hello",
    "no requested App means no normalization at all",
  );
});

test("user never gets a message glued onto a shorter App pill | Given the App `herdr` and the message `mcp hello` | When the observed turn is `@herdrmcp hello` | Then no separator means no mention and the text stays unchanged", () => {
  assert.equal(
    stripLeadingAppMentionTokens("@herdrmcp hello", ["herdr"]),
    "@herdrmcp hello",
    "an empty separator is not a pill boundary",
  );
  assert.equal(
    stripLeadingAppMentionTokens("herdrmcp hello", ["herdr"]),
    "herdrmcp hello",
    "the body may not start where the App name ends without a separator",
  );
  assert.equal(
    stripLeadingAppMentionTokens("@herdr-mcphello world", ["herdr-mcp"]),
    "@herdr-mcphello world",
    "a pill glued to the body by joined parts is not a mention",
  );
  assert.equal(
    stripLeadingAppMentionTokens("herdrmcp hello world", ["herdr-mcp"]),
    "herdrmcp hello world",
    "a different provider App id is not the requested separator-delimited name",
  );
  assert.equal(
    stripLeadingAppMentionTokens("@herdr-mcp--deep hello", ["herdr-mcp"]),
    "@herdr-mcp--deep hello",
    "a dash right after the pill is not a body separator",
  );
});

test("user never gets an unrequested App pill stripped from the late readback | Given the late-acceptance readback in wake.js | When it normalizes the stored user turn | Then only the App names the caller explicitly requested are stripped", () => {
  const start = wakeSource.indexOf("const requestedApps = Array.isArray(params.required_apps)");
  const end = wakeSource.indexOf("evidence.result.accepted_user_text_sha256", start);
  assert.ok(start >= 0 && end > start, "the late-acceptance readback must stay extractable");
  const segment = wakeSource.slice(start, end);
  assert.match(
    segment,
    /stripLeadingAppMentionTokens\(\s*full,\s*requestedApps\s*\)/,
    "the readback must strip through the requested-App list",
  );
  assert.match(
    segment,
    /const requestedApps = Array\.isArray\(params\.required_apps\) \? params\.required_apps : \[\];/,
    "the stripped App names must come from params.required_apps only",
  );
  assert.doesNotMatch(
    segment,
    /acceptedApps/,
    "an App the bubble happens to carry must never be stripped unless it was requested",
  );
  assert.match(
    segment,
    /parts\.join\("\\n"\)/,
    "stored parts are joined with a real separator so a pill part cannot glue onto the body",
  );
});
