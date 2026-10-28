import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

// #623 regression: an archived ChatGPT conversation exposes no user turn any
// more, so the registration precondition proof rejected it and the session
// could never be registered again. Every later archive readback against that
// session then failed with browser_session_target_unavailable (the tab was
// gone) or browser_session_content_dispatch_failed (the tab was reopened but
// never registered).
//
// These tests extract the real proof block from wake.js and run it, so they
// exercise the shipped logic rather than a restatement of it.

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const wakeSource = readFileSync(path.join(__dirname, "..", "extension", "content", "wake.js"), "utf8");

function extractProofBlock() {
  const anchor = "    if (ADAPTER.name === \"chatgpt\" && chatGptConversationId()) {";
  const start = wakeSource.indexOf(anchor);
  assert.ok(start >= 0, "the registration precondition proof must remain extractable");
  const end = wakeSource.indexOf("\n    const browserAppKeywords =", start);
  assert.ok(end > start, "the precondition proof block must remain bounded");
  return wakeSource.slice(start, end);
}

// Runs the extracted proof block with a stubbed provider readback.
async function runProof({ snapshot, conversation }) {
  const proofBlock = extractProofBlock();
  const source = `
    const ADAPTER = { name: "chatgpt" };
    const chatGptConversationId = () => "conv-archived-1";
    const fetchChatGptConversationSnapshot = async () => snapshot;
    const fetchChatGptConversation = async () => conversation;
    let lastBrowserRegistrationStatus = "unset";
    let registeredConvKey = null;
    let registeredBrowserSessionRef = null;
    let registeredBrowserGeneration = null;
    let registeredHerdrAppKeyword = null;
    let registeredConversationBound = false;
    const clearBrowserRegistrationScope = () => { cleared = true; };
    let cleared = false;
    const convKey = "https://chatgpt.com/c/conv-archived-1";
    const run = async () => {
      const outcome = await (async () => {
      ${proofBlock}
      })();
      return { status: lastBrowserRegistrationStatus, refused: outcome === null, cleared };
    };
    return run();
  `;
  // eslint-disable-next-line no-new-func
  const factory = new Function("snapshot", "conversation", source);
  return factory(snapshot, conversation);
}

test("#623 | Given an archived conversation | When the provider readback proves it is archived | Then the registration proof accepts it", async () => {
  const result = await runProof({
    // An archived conversation has no user turn left, so the snapshot carries
    // no userMessageId and the proof must fall through to the archived read.
    snapshot: { ok: true, userMessageId: null },
    conversation: { ok: true, body: { is_archived: true } },
  });
  assert.equal(result.status, "unset");
  assert.equal(result.cleared, false);
});

test("#623 | Given an archived conversation | When the provider readback cannot be read | Then the registration proof still refuses", async () => {
  const result = await runProof({
    snapshot: { ok: false, reason: "http-404" },
    conversation: { ok: false, reason: "http-404" },
  });
  assert.equal(result.status, "provider_readback_unavailable");
  assert.equal(result.cleared, true);
});

test("#623 | Given a live conversation | When the snapshot carries a user turn | Then the proof accepts it", async () => {
  const result = await runProof({
    snapshot: { ok: true, userMessageId: "msg-1" },
    conversation: { ok: false, reason: "should-not-be-called" },
  });
  assert.equal(result.status, "unset");
  assert.equal(result.cleared, false);
});

test("#623 | Given a conversation the provider can read but that is not archived | When neither proof holds | Then the proof refuses", async () => {
  const result = await runProof({
    snapshot: { ok: true, userMessageId: "   " },
    conversation: { ok: true, body: { is_archived: false } },
  });
  assert.equal(result.status, "provider_readback_unavailable");
  assert.equal(result.cleared, true);
});
