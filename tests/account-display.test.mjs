import test from "node:test";
import assert from "node:assert/strict";
import { formatPlatformAccountDisplay } from "../extension/account-display.js";

test("user sees a platform account label | Given a provider and a logged-in email | When the display label is formatted | Then it is provider-email or empty when the email is unusable", () => {
  assert.equal(formatPlatformAccountDisplay("ChatGPT", "Whshang1@gmail.com"), "chatgpt-whshang1@gmail.com");
  assert.equal(formatPlatformAccountDisplay("grok", "owner@example.com"), "grok-owner@example.com");
  assert.equal(formatPlatformAccountDisplay("chat gpt", "owner@example.com"), null);
  assert.equal(formatPlatformAccountDisplay("chatgpt", ""), null);
  assert.equal(formatPlatformAccountDisplay("chatgpt", "not-an-email"), null);
});
