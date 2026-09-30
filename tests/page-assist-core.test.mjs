import test from "node:test";
import assert from "node:assert/strict";
import {
  normalizeOrigin,
  originToMatchPattern,
  hasDisallowedParameters,
  createGenerationToken,
  createElementRef,
  isRefValidForGeneration,
  isDisallowedInputType,
  isSensitiveAutocomplete,
  SENSITIVE_AUTOCOMPLETE_TOKENS,
  clampVisibleText,
  validatePageAssistRequest,
} from "../extension/page-assist-core.js";

test("normalizeOrigin accepts standard http and https origins only", () => {
  assert.equal(normalizeOrigin("https://example.com/some/path?query=1"), "https://example.com");
  assert.equal(normalizeOrigin("http://localhost:8080"), "http://localhost:8080");
  assert.equal(normalizeOrigin("https://SUB.DOMAIN.ORG:443/"), "https://sub.domain.org");
  assert.equal(normalizeOrigin("javascript:alert(1)"), null);
  assert.equal(normalizeOrigin("chrome-extension://xyz"), null);
  assert.equal(normalizeOrigin("file:///etc/passwd"), null);
  assert.equal(normalizeOrigin("data:text/html,test"), null);
  assert.equal(normalizeOrigin(""), null);
  assert.equal(normalizeOrigin(null), null);
});

test("originToMatchPattern converts normalized origin to wildcard match pattern", () => {
  assert.equal(originToMatchPattern("https://example.com/app"), "https://example.com/*");
  assert.equal(originToMatchPattern("http://localhost:3000"), "http://localhost:3000/*");
  assert.equal(originToMatchPattern("invalid-url"), null);
});

test("hasDisallowedParameters detects forbidden caller inputs", () => {
  assert.equal(hasDisallowedParameters({ action: "inspect" }), false);
  assert.equal(hasDisallowedParameters({ action: "click", ref: "ref_1" }), false);
  assert.equal(hasDisallowedParameters({ selector: "#btn" }), true);
  assert.equal(hasDisallowedParameters({ xpath: "//button" }), true);
  assert.equal(hasDisallowedParameters({ js: "alert(1)" }), true);
  assert.equal(hasDisallowedParameters({ script: "..." }), true);
  assert.equal(hasDisallowedParameters({ eval: "..." }), true);
  assert.equal(hasDisallowedParameters({ cookie: "..." }), true);
  assert.equal(hasDisallowedParameters({ storage: "..." }), true);
  assert.equal(hasDisallowedParameters({ nested: { cdp: true } }), true);
});

test("user can target any web origin | Given install-time all-sites access | When a Page Assist request is validated | Then http and https origins pass shape validation and non-web schemes fail closed", () => {
  const badAction = validatePageAssistRequest({ action: "delete", targetOrigin: "https://app.test" });
  assert.equal(badAction.ok, false);
  assert.equal(badAction.error, "invalid_action");

  const arbitraryWebOrigin = validatePageAssistRequest({
    action: "inspect",
    targetOrigin: "https://example.net/path",
  });
  assert.equal(arbitraryWebOrigin.ok, true);
  assert.equal(arbitraryWebOrigin.targetOrigin, "https://example.net");

  const nonWebOrigin = validatePageAssistRequest({ action: "inspect", targetOrigin: "chrome://extensions" });
  assert.equal(nonWebOrigin.ok, false);
  assert.equal(nonWebOrigin.error, "target_origin_required");

  const forbiddenParam = validatePageAssistRequest({
    action: "inspect",
    targetOrigin: "https://app.test",
    selector: "div",
  });
  assert.equal(forbiddenParam.ok, false);
  assert.equal(forbiddenParam.error, "disallowed_parameter");

  const validInspect = validatePageAssistRequest({ action: "inspect", targetOrigin: "https://app.test" });
  assert.equal(validInspect.ok, true);
  assert.equal(validInspect.targetOrigin, "https://app.test");
  assert.equal(typeof validInspect.maxChars, "number");
});

test("isSensitiveAutocomplete detects sensitive credential and card fields", () => {
  const sensitiveList = ["current-password", "new-password", "one-time-code", "cc-number", "cc-csc"];
  for (const token of sensitiveList) {
    assert.equal(SENSITIVE_AUTOCOMPLETE_TOKENS.includes(token), true);
    assert.equal(isSensitiveAutocomplete(token), true);
    assert.equal(isSensitiveAutocomplete(`section-blue ${token} extra`), true);
  }
  assert.equal(isSensitiveAutocomplete("username"), false);
  assert.equal(isSensitiveAutocomplete("email"), false);
  assert.equal(isSensitiveAutocomplete("tel"), false);
  assert.equal(isSensitiveAutocomplete(""), false);
  assert.equal(isSensitiveAutocomplete(null), false);
});

test("validatePageAssistRequest validates generation and ref for click and fill", () => {
  const genToken = createGenerationToken(1);
  const validRef = createElementRef(genToken, 0);

  assert.equal(
    validatePageAssistRequest({ action: "click", targetOrigin: "https://app.test", generation: genToken }).ok,
    false
  );

  assert.equal(
    validatePageAssistRequest({ action: "click", targetOrigin: "https://app.test", ref: validRef }).ok,
    false
  );

  const stale = validatePageAssistRequest({
    action: "click",
    targetOrigin: "https://app.test",
    ref: "ref_othergen_0",
    generation: genToken,
  });
  assert.equal(stale.ok, false);
  assert.equal(stale.error, "stale_ref");

  const validClick = validatePageAssistRequest({
    action: "click",
    targetOrigin: "https://app.test",
    ref: validRef,
    generation: genToken,
  });
  assert.equal(validClick.ok, true);

  const missingVal = validatePageAssistRequest({
    action: "fill",
    targetOrigin: "https://app.test",
    ref: validRef,
    generation: genToken,
  });
  assert.equal(missingVal.ok, false);
  assert.equal(missingVal.error, "value_required");

  const validFill = validatePageAssistRequest({
    action: "fill",
    targetOrigin: "https://app.test",
    ref: validRef,
    generation: genToken,
    value: "hello world",
  });
  assert.equal(validFill.ok, true);
  assert.equal(validFill.value, "hello world");
});

test("isDisallowedInputType filters out password and file upload inputs", () => {
  assert.equal(isDisallowedInputType("password"), true);
  assert.equal(isDisallowedInputType("file"), true);
  assert.equal(isDisallowedInputType("hidden"), true);
  assert.equal(isDisallowedInputType("text"), false);
  assert.equal(isDisallowedInputType("search"), false);
  assert.equal(isDisallowedInputType("email"), false);
});

test("clampVisibleText bounds text length deterministically", () => {
  const longText = "a".repeat(50000);
  const clamped = clampVisibleText(longText, 1000);
  assert.equal(clamped.length, 1000);
  assert.equal(clampVisibleText("  short  "), "short");
});
