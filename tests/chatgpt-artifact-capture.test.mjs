import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import {
  bindingAllowsArtifactCapture,
  captureSenderContext,
  normalizeCaptureArtifact,
} from "../extension/artifact-capture-gate.js";

await import("../extension/content/chatgpt-artifact-core.js");
const core = globalThis.HerdrChatGptArtifactCore;
const here = dirname(fileURLToPath(import.meta.url));
const root = join(here, "..");

function pngBytes() {
  return new Uint8Array([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 0]);
}

function artifact(overrides = {}) {
  return {
    conversation_id: "6a94508d-7f64-83ea-86f1-45a6685cee08",
    file_id: "00000000cc1c81fd8ee18b5f0913cf46",
    mime: "image/png",
    bytes_b64: "iVBORw0KGgo=",
    sha256: "a".repeat(64),
    ...overrides,
  };
}

test("ChatGPT artifact core scopes the real image-only payload to the latest user turn", () => {
  assert.ok(core);
  const payload = {
    messages: [
      { id: "u0", author: { role: "user" }, content: { parts: ["old draw"] } },
      {
        id: "a0",
        author: { role: "assistant" },
        status: "finished_successfully",
        end_turn: true,
        content: { parts: [
          { content_type: "image_asset_pointer", asset_pointer: "sediment://file_oldimage000000000000000000000001" },
        ] },
      },
      { id: "u1", author: { role: "user" }, create_time: 100, content: { parts: ["draw new"] } },
      {
        id: "tool1",
        author: { role: "tool" },
        status: "finished_successfully",
        content: { content_type: "multimodal_text", parts: [
          { content_type: "image_asset_pointer", asset_pointer: "sediment://file_00000000cc1c81fd8ee18b5f0913cf46" },
          { content_type: "image_asset_pointer", asset_pointer: "sediment://file_00000000cc1c81fd8ee18b5f0913cf46" },
        ] },
      },
      {
        id: "a1",
        author: { role: "assistant" },
        create_time: 102,
        status: "finished_successfully",
        end_turn: false,
        content: { content_type: "reasoning_recap", parts: [] },
      },
    ],
  };
  assert.deepEqual(core.imageFileIdsFromConversation(payload), ["00000000cc1c81fd8ee18b5f0913cf46"]);
  const turn = core.latestTurnMessages(payload);
  assert.equal(turn.user.id, "u1");
  assert.equal(turn.assistant.id, "a1");
  assert.equal(turn.currentRole, "assistant");
  assert.equal(core.parseFileIdFromPart({ content_type: "image_asset_pointer", asset_pointer: "sediment://file_bad/slash" }), null);
});

test("image-only settled capture key is stable and fail-closed", () => {
  const base = {
    adapterName: "chatgpt",
    conversationId: "6a94508d-7f64-83ea-86f1-45a6685cee08",
    pending: true,
    submitAt: 1234,
    server: {
      ok: true,
      currentNodeRole: "assistant",
      finished: true,
      messageId: "assistant-image-recap-1",
      text: "",
      imageFileIds: ["00000000cc1c81fd8ee18b5f0913cf46"],
    },
  };
  const key = core.settledImageCaptureKey(base);
  assert.ok(key);
  assert.equal(core.settledImageCaptureKey(base), key, "duplicate watcher ticks use the same once-fence key");
  assert.equal(core.settledImageCaptureKey({ ...base, pending: false }), null);
  assert.equal(core.settledImageCaptureKey({ ...base, adapterName: "claude" }), null);
  assert.equal(core.settledImageCaptureKey({ ...base, server: { ...base.server, finished: false } }), null);
  assert.equal(core.settledImageCaptureKey({ ...base, server: { ...base.server, imageFileIds: [] } }), null);
});

test("ChatGPT authenticated download target is pinned to HTTPS chatgpt.com estuary", () => {
  const good = "https://chatgpt.com/backend-api/estuary/content?id=x&ts=1&sig=y";
  assert.equal(core.isChatGptDownloadUrl(good), true);
  assert.equal(core.isChatGptDownloadUrl(good.replace("https:", "http:")), false);
  assert.equal(core.isChatGptDownloadUrl("https://evil.example/backend-api/estuary/content?id=x"), false);
  assert.equal(core.isChatGptDownloadUrl("https://chatgpt.com.evil.example/backend-api/estuary/content?id=x"), false);
  assert.equal(core.isChatGptDownloadUrl("https://user:pass@chatgpt.com/backend-api/estuary/content?id=x"), false);
  assert.equal(core.isChatGptDownloadUrl("https://chatgpt.com:444/backend-api/estuary/content?id=x"), false);
  assert.equal(core.isChatGptDownloadUrl("https://oaiusercontent.com/file.png"), false);
  assert.equal(core.isChatGptDownloadUrl("https://chatgpt.com/backend-api/files/download/file_x"), false);
});

test("image MIME and native artifact shape fail closed", async () => {
  const png = pngBytes();
  assert.equal(core.magicMime(png), "image/png");
  assert.equal(core.resolvedImageMime("image/png", png), "image/png");
  assert.equal(core.resolvedImageMime("image/jpeg", png), null);
  assert.equal(core.resolvedImageMime("text/html", png), null);
  const msg = core.nativeArtifactMessage(artifact());
  assert.ok(msg);
  assert.deepEqual(Object.keys(msg).sort(), ["bytes_b64", "conversation_id", "file_id", "mime", "sha256"]);
  assert.equal(core.isBoundarySafeCaptureMessage({ ...artifact(), authorization: "Bearer secret" }), false);
  assert.equal(core.isBoundarySafeCaptureMessage({ ...artifact(), download_url: "https://chatgpt.com/backend-api/estuary/content" }), false);
  assert.equal(core.nativeArtifactMessage(artifact({ mime: "image/svg+xml" })), null);
  assert.equal(core.nativeArtifactMessage(artifact({ bytes_b64: "A".repeat(Math.ceil((core.RAW_LIMIT_BYTES * 4) / 3) + 20) })), null);
  assert.equal((await core.sha256Hex(png)).length, 64);
});

test("background artifact admission binds sender conversation and tab to current Herdr scope", () => {
  const a = normalizeCaptureArtifact(artifact());
  assert.ok(a);
  assert.equal(normalizeCaptureArtifact({ ...artifact(), token: "secret" }), null);
  assert.equal(normalizeCaptureArtifact({ ...artifact(), download_url: "https://chatgpt.com/" }), null);

  const directUrl = "https://chatgpt.com/c/6a94508d-7f64-83ea-86f1-45a6685cee08";
  const info = captureSenderContext(directUrl, a.conversation_id);
  assert.ok(info);
  assert.equal(captureSenderContext(directUrl, "other-conversation"), null);
  assert.equal(captureSenderContext("https://evil.example/c/6a94508d-7f64-83ea-86f1-45a6685cee08", a.conversation_id), null);
  assert.equal(bindingAllowsArtifactCapture({ binding_scope: "conversation", convKey: info.convKey, tabId: 42 }, info, 42), true);
  assert.equal(bindingAllowsArtifactCapture({ binding_scope: "conversation", convKey: info.convKey, tabId: 41 }, info, 42), false);

  const projectUrl = "https://chatgpt.com/g/g-p-demo/c/6a94508d-7f64-83ea-86f1-45a6685cee08";
  const projectInfo = captureSenderContext(projectUrl, a.conversation_id);
  assert.ok(projectInfo);
  assert.equal(bindingAllowsArtifactCapture({
    binding_scope: "project",
    project_id: projectInfo.project_id,
    convKey: projectInfo.project_key,
    active_conv_key: projectInfo.convKey,
    tabId: 42,
  }, projectInfo, 42), true);
  assert.equal(bindingAllowsArtifactCapture({
    binding_scope: "project",
    project_id: projectInfo.project_id,
    convKey: projectInfo.project_key,
    active_conv_key: `${projectInfo.project_key}/c/stale`,
    tabId: 42,
  }, projectInfo, 42), false);
});

test("extension source keeps Bearer private while both authenticated download stages use it", async () => {
  const wake = await readFile(join(root, "extension/content/wake.js"), "utf8");
  const background = await readFile(join(root, "extension/background.js"), "utf8");
  const localAuth = await readFile(join(root, "extension/local-auth.js"), "utf8");
  const manifest = JSON.parse(await readFile(join(root, "extension/manifest.json"), "utf8"));

  assert.equal(manifest.version, "0.1.84");
  const chatgpt = manifest.content_scripts.find((entry) => entry.matches.includes("https://chatgpt.com/*"));
  assert.ok(chatgpt);
  assert.ok(chatgpt.js.indexOf("content/chatgpt-artifact-core.js") < chatgpt.js.indexOf("content/wake.js"));
  assert.match(wake, /\/api\/auth\/session/);
  assert.match(wake, /CORE\.conversationsUrl/);
  assert.match(wake, /CORE\.fileDownloadUrl/);
  assert.match(wake, /CORE\.isChatGptDownloadUrl/);
  assert.ok((wake.match(/redirect: "error"/g) || []).length >= 4, "authenticated ChatGPT fetches fail closed on redirects");
  assert.match(wake, /headers: \{ accept: "\*\/\*", authorization: `Bearer \$\{sessionToken\}` \}/);
  assert.match(wake, /const result = await sendBg\(\{ type: "h2w_artifact_capture", artifact \}\);/);
  assert.doesNotMatch(wake, /void sendBg\(\{ type: "h2w_artifact_capture", artifact \}\);/);
  assert.match(wake, /void maybeCaptureChatGptTurnImages\(\);/);
  assert.match(wake, /ARTIFACT_CAPTURE_RETRY_DELAYS_MS = \[0, 1200, 3000\]/);
  assert.match(wake, /scheduleServerSettledImageCapture\(server\)/);
  assert.match(wake, /if \(!reported && captureScheduled\)/);
  const matcherStart = wake.indexOf("const serverSnapshotMatchesPendingTurn");
  const matcherEnd = wake.indexOf("const maybeReportServerSettledTurn", matcherStart);
  assert.ok(matcherStart > 0 && matcherEnd > matcherStart);
  assert.doesNotMatch(wake.slice(matcherStart, matcherEnd), /server\.text/, "image-only settlement must not require assistant text");
  assert.match(wake, /if \(!normalizedAssistant \|\| !looksLikeSubstantiveReply\(normalizedAssistant\)\) return false;/,
    "text continuity remains substantive-only");
  assert.match(background, /msg\?\.type === "h2w_artifact_capture"/);
  assert.match(background, /bindingAllowsArtifactCapture/);
  assert.match(background, /captureWebArtifactNative\(artifact\)/);
  assert.match(localAuth, /type: "artifact_capture"/);
  assert.doesNotMatch(localAuth, /download_url/);
});
