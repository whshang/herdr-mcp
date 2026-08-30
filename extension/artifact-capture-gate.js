// artifact-capture-gate.js — pure admission checks for browser -> native image capture.
// No network/storage/native side effects; Node tests exercise the same gate used by background.js.
import { chatGptConversationInfo } from "./continuity-core.js";

const ALLOWED_FIELDS = ["conversation_id", "file_id", "mime", "bytes_b64", "sha256"];
const IMAGE_MIMES = new Set(["image/png", "image/jpeg", "image/gif", "image/webp"]);
const MAX_BASE64_CHARS = Math.ceil(((8 * 1024 * 1024) * 4) / 3) + 8;
const ID_RE = /^[A-Za-z0-9_.:=+-]+$/;

function validId(value, min, max) {
  return typeof value === "string"
    && value.length >= min
    && value.length <= max
    && ID_RE.test(value);
}

export function normalizeCaptureArtifact(value) {
  if (!value || typeof value !== "object" || Array.isArray(value)) return null;
  const keys = Object.keys(value);
  if (keys.length < 4 || keys.length > ALLOWED_FIELDS.length) return null;
  if (keys.some((key) => !ALLOWED_FIELDS.includes(key))) return null;
  if (!validId(value.conversation_id, 1, 128)) return null;
  if (!validId(value.file_id, 4, 256)) return null;
  if (!IMAGE_MIMES.has(value.mime)) return null;
  if (typeof value.bytes_b64 !== "string" || value.bytes_b64.length === 0 || value.bytes_b64.length > MAX_BASE64_CHARS) return null;
  if (value.sha256 != null && (typeof value.sha256 !== "string" || !/^[0-9a-f]{64}$/i.test(value.sha256))) return null;
  const artifact = {
    conversation_id: value.conversation_id,
    file_id: value.file_id,
    mime: value.mime,
    bytes_b64: value.bytes_b64,
  };
  if (value.sha256 != null) artifact.sha256 = value.sha256;
  return artifact;
}

export function captureSenderContext(rawUrl, artifactConversationId) {
  const info = chatGptConversationInfo(rawUrl);
  if (!info || info.site !== "chatgpt" || !info.conversation_id) return null;
  if (info.conversation_id !== artifactConversationId) return null;
  return info;
}

export function bindingAllowsArtifactCapture(binding, senderInfo, senderTabId) {
  if (!binding || !senderInfo?.convKey || !Number.isInteger(senderTabId) || senderTabId <= 0) return false;
  const projectScoped = binding.binding_scope === "project" && Boolean(binding.project_id);
  const deliveryKey = projectScoped ? binding.active_conv_key : binding.convKey;
  if (deliveryKey !== senderInfo.convKey) return false;
  return Number(binding.tabId || 0) === senderTabId;
}

export const ARTIFACT_CAPTURE_ALLOWED_FIELDS = Object.freeze([...ALLOWED_FIELDS]);
