import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const backgroundSource = readFileSync(path.join(__dirname, "..", "extension", "background.js"), "utf8");
const wakeSource = readFileSync(path.join(__dirname, "..", "extension", "content", "wake.js"), "utf8");

const recoveryStart = backgroundSource.indexOf("async function recoverBrowserSessionTarget(");
const recoveryEnd = backgroundSource.indexOf("\nasync function handleBrowserActuation", recoveryStart);
assert.ok(recoveryStart >= 0 && recoveryEnd > recoveryStart, "recovery helper must remain extractable");
const recoverySource = backgroundSource.slice(recoveryStart, recoveryEnd);

function recoveryHarness(tabRecords) {
  const browserSessionTargets = new Map();
  const chrome = {
    tabs: {
      async query() {
        return tabRecords.map(({ id, url, status = "complete" }) => ({ id, url, status }));
      },
      async sendMessage(tabId, message) {
        assert.equal(message?.type, "h2w_get_convkey");
        const record = tabRecords.find((item) => item.id === tabId);
        if (!record || record.error) throw new Error("missing content listener");
        return record.live;
      },
    },
  };
  const activeH2WTabUrls = () => ["https://chatgpt.com/*"];
  const browserConversationInfoFromSupportedUrl = (rawUrl) => {
    const match = String(rawUrl || "").match(/^https:\/\/chatgpt\.com\/c\/([^/?#]+)/);
    if (!match) return null;
    return {
      site: "chatgpt",
      conversation_id: match[1],
      project_id: null,
      convKey: `https://chatgpt.com/c/${match[1]}`,
    };
  };
  const recover = new Function(
    "chrome",
    "activeH2WTabUrls",
    "browserConversationInfoFromSupportedUrl",
    "browserSessionTargets",
    `${recoverySource}; return recoverBrowserSessionTarget;`,
  )(chrome, activeH2WTabUrls, browserConversationInfoFromSupportedUrl, browserSessionTargets);
  return { recover, browserSessionTargets };
}

test("service-worker recovery rebuilds exactly one stable session target without page mutation", async () => {
  const sessionRef = "br_session_recovery";
  const { recover, browserSessionTargets } = recoveryHarness([{
    id: 41,
    url: "https://chatgpt.com/c/recovery-1",
    live: {
      convKey: "https://chatgpt.com/c/recovery-1",
      url: "https://chatgpt.com/c/recovery-1",
      site: "chatgpt",
      browserSessionRef: sessionRef,
      browserGeneration: 7,
    },
  }]);

  const recovered = await recover(sessionRef, 7);
  assert.equal(recovered.ambiguous, false);
  assert.equal(recovered.observedGeneration, 7);
  assert.equal(recovered.target?.tabId, 41);
  assert.equal(recovered.target?.conversationId, "recovery-1");
  assert.equal(recovered.target?.observationGeneration, 7);
  assert.deepEqual(browserSessionTargets.get(sessionRef), recovered.target);
  assert.doesNotMatch(recoverySource, /tabs\.reload|scripting\.executeScript|tabs\.update/);
});

test("service-worker recovery fails closed when the same logical session is open twice", async () => {
  const sessionRef = "br_duplicate_session";
  const live = {
    convKey: "https://chatgpt.com/c/duplicate",
    url: "https://chatgpt.com/c/duplicate",
    site: "chatgpt",
    browserSessionRef: sessionRef,
    browserGeneration: 7,
  };
  const { recover, browserSessionTargets } = recoveryHarness([
    { id: 51, url: live.url, live },
    { id: 52, url: live.url, live },
  ]);

  const recovered = await recover(sessionRef, 7);
  assert.equal(recovered.target, null);
  assert.equal(recovered.ambiguous, true);
  assert.equal(browserSessionTargets.has(sessionRef), false);
});

test("service-worker recovery reports a newer generation instead of reusing a stale target", async () => {
  const sessionRef = "br_stale_session";
  const { recover, browserSessionTargets } = recoveryHarness([{
    id: 61,
    url: "https://chatgpt.com/c/stale",
    live: {
      convKey: "https://chatgpt.com/c/stale",
      url: "https://chatgpt.com/c/stale",
      site: "chatgpt",
      browserSessionRef: sessionRef,
      browserGeneration: 8,
    },
  }]);

  const recovered = await recover(sessionRef, 7);
  assert.equal(recovered.target, null);
  assert.equal(recovered.ambiguous, false);
  assert.equal(recovered.observedGeneration, 8);
  assert.equal(browserSessionTargets.has(sessionRef), false);
});

test("page identity handshake exposes only opaque Browser Registry recovery identity", () => {
  assert.match(wakeSource, /browserSessionRef:\s*registeredBrowserSessionRef/);
  assert.match(wakeSource, /browserGeneration:\s*registeredBrowserGeneration/);
  assert.match(wakeSource, /registeredBrowserSessionRef\s*=\s*null;\s*\n\s*registeredBrowserGeneration\s*=\s*null;/);
  assert.doesNotMatch(
    wakeSource.slice(wakeSource.indexOf('if \(msg?.type === "h2w_get_convkey"\)'), wakeSource.indexOf('if \(msg?.type === "h2w_snapshot_turn"\)')),
    /accountNativeIdentity|email|userId/i,
  );
});

