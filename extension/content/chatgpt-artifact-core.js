// chatgpt-artifact-core.js — pure protocol helpers for ChatGPT Web generated-image
// artifact capture. Plain IIFE (no ES modules): loaded as a content script before
// wake.js and reused by Node tests through the guarded CommonJS export below.
// Direction: web → herdr. These helpers only parse and validate; they never
// fetch, never touch tokens/cookies, and never log anything.
//
// Field contract: the only values allowed to cross the native boundary are
// conversation_id, file_id, mime, bytes_b64, sha256. Everything else (session
// token, cookies, download URLs) must stay inside the ChatGPT page context.

(function (global) {
  "use strict";

  const RAW_LIMIT_BYTES = 8 * 1024 * 1024;
  const MAX_FILES_PER_TURN = 8;
  const SIMPLE_NATIVE_FIELDS = [
    "conversation_id",
    "file_id",
    "mime",
    "bytes_b64",
    "sha256",
  ];

  function isPlainObject(value) {
    return value !== null && typeof value === "object" && !Array.isArray(value);
  }

  // ---- conversation payload parsing -------------------------------------

  // Safely collect strings/maps from a ChatGPT message content.parts array,
  // including the finalized-turn layout used by the plural conversations API.
  function partsOf(message) {
    if (!isPlainObject(message)) return [];
    const parts = message.content && message.content.parts;
    return Array.isArray(parts) ? parts : [];
  }

  function latestTurnMessages(payload) {
    const messages = isPlainObject(payload) && Array.isArray(payload.messages) ? payload.messages : null;
    if (!messages) return null;
    const current = messages.length ? messages[messages.length - 1] : null;
    const currentRole = String(current?.author?.role || "");
    let assistantIndex = -1;
    for (let i = messages.length - 1; i >= 0; i -= 1) {
      if (messages[i]?.author?.role === "assistant") { assistantIndex = i; break; }
    }
    const assistant = assistantIndex >= 0 ? messages[assistantIndex] : null;
    let user = currentRole === "user" ? current : null;
    if (!user) {
      const start = assistantIndex >= 0 ? assistantIndex - 1 : messages.length - 1;
      for (let i = start; i >= 0; i -= 1) {
        if (messages[i]?.author?.role === "user") { user = messages[i]; break; }
      }
    }
    return { messages, current, currentRole, assistant, user };
  }

  // Parse an individual part; returns a file_id only when it is a well-formed
  // image_asset_pointer with a `sediment://file_<id>` pointer.
  function parseFileIdFromPart(part) {
    if (!isPlainObject(part)) return null;
    if (part.content_type !== "image_asset_pointer") return null;
    const pointer = String(part.asset_pointer || "");
    const match = pointer.match(/^sediment:\/\/file_([A-Za-z0-9_-]{4,256})$/);
    return match ? match[1] : null;
  }

  // Extract unique image-asset file_ids from the CURRENT turn only. For the
  // plural conversations shape, the turn starts at the latest user message and
  // includes any following tool/image/assistant messages. For the legacy
  // mapping shape, follow the current-node parent chain back to the latest user.
  // This deliberately refuses to scan older turns: a newly-settled image-only
  // turn must never capture a historical image just because it is still in the
  // conversation payload. Bounded to MAX_FILES_PER_TURN.
  function imageFileIdsFromConversation(payload) {
    const seen = new Set();
    const found = [];
    const push = (part) => {
      const fileId = parseFileIdFromPart(part);
      if (!fileId || seen.has(fileId) || found.length >= MAX_FILES_PER_TURN) return;
      seen.add(fileId);
      found.push(fileId);
    };
    if (isPlainObject(payload)) {
      if (Array.isArray(payload.messages)) {
        let latestUserIndex = -1;
        for (let i = payload.messages.length - 1; i >= 0; i -= 1) {
          if (payload.messages[i]?.author?.role === "user") {
            latestUserIndex = i;
            break;
          }
        }
        const scopedMessages = latestUserIndex >= 0
          ? payload.messages.slice(latestUserIndex)
          : [];
        for (const message of scopedMessages) {
          for (const part of partsOf(message)) push(part);
        }
        return found;
      }
      if (isPlainObject(payload.mapping) && typeof payload.current_node === "string") {
        const currentTurn = [];
        let nodeId = payload.current_node;
        for (let i = 0; nodeId && i < 80; i += 1) {
          const node = payload.mapping[nodeId];
          if (!isPlainObject(node)) break;
          const message = node.message;
          if (isPlainObject(message)) currentTurn.push(message);
          if (message?.author?.role === "user") break;
          nodeId = node.parent || null;
        }
        currentTurn.reverse();
        if (currentTurn[0]?.author?.role !== "user") return found;
        for (const message of currentTurn) {
          for (const part of partsOf(message)) push(part);
        }
      }
    }
    return found;
  }

  // Pure eligibility/key helper for the watcher. Text completion and image
  // capture intentionally have different gates: an image-only ChatGPT turn can
  // be server-finished with empty assistant text. The returned key is stable for
  // duplicate watcher ticks and is suitable for a bounded in-memory once fence.
  function settledImageCaptureKey({
    adapterName,
    conversationId,
    pending,
    submitAt,
    server,
  } = {}) {
    if (adapterName !== "chatgpt" || !pending || !validConversationId(conversationId)) return null;
    if (!isPlainObject(server) || server.ok !== true) return null;
    if (server.currentNodeRole !== "assistant" || server.finished !== true) return null;
    const fileIds = Array.isArray(server.imageFileIds)
      ? server.imageFileIds.filter((fileId) => validFileId(fileId)).slice(0, MAX_FILES_PER_TURN)
      : [];
    if (!fileIds.length) return null;
    const turnIdentity = String(server.messageId || server.updatedAt || server.createdAt || "");
    if (!turnIdentity) return null;
    const submit = Number(submitAt || 0);
    return `${conversationId}:${submit > 0 ? submit : 0}:${turnIdentity}:${fileIds.join(",")}`;
  }

  // ---- address/link builders and endpoint path validation ----------------

  function conversationsUrl(conversationId, numTurns = 40) {
    return `/backend-api/conversations/${encodeURIComponent(conversationId)}?include_has_versions=true&num_turns=${numTurns}`;
  }

  function fileDownloadUrl(fileId, conversationId) {
    return `/backend-api/files/download/${encodeURIComponent(fileId)}?include_library_file_state=true&conversation_id=${encodeURIComponent(conversationId)}&inline=false&download_intent=false`;
  }

  function isChatGptDownloadUrl(value) {
    if (typeof value !== "string" || !value || value.length > 8192) return false;
    try {
      const url = new URL(value);
      return url.origin === "https://chatgpt.com"
        && !url.username
        && !url.password
        && url.pathname === "/backend-api/estuary/content";
    } catch (_) {
      return false;
    }
  }

  // ---- identifier validation ----------------------------------------------

  // Same path-safe rule enforced on the Rust side: no separators, whitespace,
  // control bytes, or reserved delimiters.
  const ID_BYTES = new Set("ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_.:=+".split(""));
  function validId(value, min, max) {
    if (typeof value !== "string") return false;
    if (value.length < min || value.length > max) return false;
    for (let i = 0; i < value.length; i += 1) {
      if (!ID_BYTES.has(value[i])) return false;
    }
    return true;
  }

  function validConversationId(value) {
    return validId(value, 1, 128);
  }

  function validFileId(value) {
    return validId(value, 4, 256);
  }

  function validSha256(value) {
    return typeof value === "string" && /^[0-9a-f]{64}$/i.test(value);
  }

  // ---- MIME / magic detection ---------------------------------------------

  const IMAGE_MIMES = new Set(["image/png", "image/jpeg", "image/gif", "image/webp"]);

  function baseMime(contentType) {
    if (typeof contentType !== "string") return "";
    return String(contentType).split(";")[0].trim().toLowerCase();
  }

  function isImageMime(contentType) {
    return IMAGE_MIMES.has(baseMime(contentType));
  }

  // Inferred from magic bytes when the server Content-Type is generic or absent.
  function magicMime(bytes) {
    if (!bytes || bytes.length < 12) return null;
    if (bytes[0] === 0x89 && bytes[1] === 0x50 && bytes[2] === 0x4e && bytes[3] === 0x47) {
      return "image/png";
    }
    if (bytes[0] === 0xff && bytes[1] === 0xd8 && bytes[2] === 0xff) {
      return "image/jpeg";
    }
    if (
      (bytes[0] === 0x47 && bytes[1] === 0x49 && bytes[2] === 0x46 && bytes[3] === 0x38)
      && (bytes[4] === 0x37 || bytes[4] === 0x39)
    ) {
      return "image/gif";
    }
    if (bytes[0] === 0x52 && bytes[1] === 0x49 && bytes[2] === 0x46 && bytes[3] === 0x46
      && bytes[8] === 0x57 && bytes[9] === 0x45 && bytes[10] === 0x42 && bytes[11] === 0x50) {
      return "image/webp";
    }
    return null;
  }

  // Decide the final image MIME for the native boundary: prefer an explicit
  // image Content-Type, fall back to magic, and reject anything that is not a
  // supported image. Returns null on any ambiguity or mismatch.
  function resolvedImageMime(contentType, bytes) {
    const declared = baseMime(contentType);
    const fromMagic = magicMime(bytes) || null;
    if (declared && IMAGE_MIMES.has(declared)) {
      if (fromMagic && fromMagic !== declared) return null; // MIME/magic mismatch
      return declared;
    }
    if (declared && !isImageMime(declared)) return null; // explicitly non-image
    return fromMagic;
  }

  // ---- byte helpers (browser-side) ----------------------------------------

  async function sha256Hex(bytes) {
    const digest = await globalThis.crypto.subtle.digest("SHA-256", bytes);
    return Array.from(new Uint8Array(digest)).map((b) => b.toString(16).padStart(2, "0")).join("");
  }

  function bytesToBase64(bytes) {
    const chunk = 0x8000;
    let binary = "";
    for (let i = 0; i < bytes.length; i += chunk) {
      binary += String.fromCharCode.apply(null, bytes.subarray(i, i + chunk));
    }
    return globalThis.btoa(binary);
  }

  // ---- native-boundary shape gate ------------------------------------------

  // A captured artifact ready to cross the native boundary. Strictly rejects
  // any extra field (token/cookie/authorization/download URL must never cross).
  function nativeArtifactMessage({ conversation_id, file_id, mime, bytes_b64, sha256 }) {
    if (!validConversationId(conversation_id)) return null;
    if (!validFileId(file_id)) return null;
    if (typeof bytes_b64 !== "string" || bytes_b64.length === 0) return null;
    if (bytes_b64.length > Math.ceil((RAW_LIMIT_BYTES * 4) / 3) + 8) return null;
    if (!IMAGE_MIMES.has(mime)) return null;
    if (sha256 != null && !validSha256(sha256)) return null;
    const output = { conversation_id, file_id, mime, bytes_b64 };
    if (sha256 != null) output.sha256 = sha256;
    return output;
  }

  // Validate a message received from a content script before Background forwards
  // it to the native host. Returns true only for the exact allowed shape.
  function isBoundarySafeCaptureMessage(message) {
    if (!isPlainObject(message)) return false;
    const keys = Object.keys(message);
    if (keys.length > SIMPLE_NATIVE_FIELDS.length) return false;
    for (const key of keys) {
      if (!SIMPLE_NATIVE_FIELDS.includes(key)) return false;
    }
    return nativeArtifactMessage(message) !== null;
  }

  const api = {
    RAW_LIMIT_BYTES,
    MAX_FILES_PER_TURN,
    SIMPLE_NATIVE_FIELDS,
    latestTurnMessages,
    parseFileIdFromPart,
    imageFileIdsFromConversation,
    settledImageCaptureKey,
    conversationsUrl,
    fileDownloadUrl,
    isChatGptDownloadUrl,
    validConversationId,
    validFileId,
    validSha256,
    baseMime,
    isImageMime,
    magicMime,
    resolvedImageMime,
    sha256Hex,
    bytesToBase64,
    nativeArtifactMessage,
    isBoundarySafeCaptureMessage,
  };

  global.HerdrChatGptArtifactCore = api;
  if (typeof module !== "undefined" && module.exports) {
    module.exports = api;
  }
})(typeof globalThis !== "undefined" ? globalThis : global);