// continuity-core.js — pure ChatGPT conversation rollover helpers.
//
// A rollover does not try to alter ChatGPT's internal context compaction.
// Instead, the current web model writes a compact handoff packet, the extension
// seeds that packet into a fresh conversation in the same Project, then the
// existing Herdr workspace bindings move atomically to the new conversation.

export const HANDOFF_VERSION = 1;
export const HANDOFF_BEGIN = "<<<HERDR_HANDOFF_V1";
export const HANDOFF_END = "<<<END_HERDR_HANDOFF_V1>>>";
export const HANDOFF_SEED_PREFIX = "[HERDR_CONTINUITY_TRANSFER";
export const MAX_HANDOFF_PACKET_CHARS = 24000;

const CHATGPT_HOSTS = new Set(["chatgpt.com", "www.chatgpt.com"]);
const PROJECT_RESOURCE_RE = /^(g-p-[0-9a-f]{32})(?:-[^/]*)?$/i;

function normalizedPathname(pathname) {
  return String(pathname || "/").replace(/\/+$/, "") || "/";
}

function stableProjectId(segment) {
  const raw = String(segment || "");
  const m = raw.match(PROJECT_RESOURCE_RE);
  return m ? m[1] : raw;
}

/**
 * Parse a real ChatGPT conversation URL and normalize Project aliases.
 *
 * ChatGPT may expose the same Project as either `g-p-<id>` or
 * `g-p-<id>-<slug>`. Bindings use the resource id only, so a cosmetic Project
 * slug change cannot orphan a conversation binding.
 */
export function chatGptConversationInfo(rawUrl) {
  try {
    const u = new URL(String(rawUrl || ""));
    if (!CHATGPT_HOSTS.has(u.hostname.toLowerCase())) return null;
    const pathname = normalizedPathname(u.pathname);

    const normal = pathname.match(/^\/c\/([^/]+)$/);
    if (normal) {
      const conversationId = normal[1];
      return {
        site: "chatgpt",
        convKey: `${u.origin}/c/${conversationId}`,
        url: u.href,
        conversation_id: conversationId,
        project_id: null,
        project_key: null,
        project_launch_url: null,
      };
    }

    const project = pathname.match(/^\/g\/(g-p-[^/]+)\/c\/([^/]+)$/i);
    if (!project) return null;
    const projectId = stableProjectId(project[1]);
    const conversationId = project[2];
    const projectKey = `${u.origin}/g/${projectId}`;
    return {
      site: "chatgpt",
      convKey: `${projectKey}/c/${conversationId}`,
      url: u.href,
      conversation_id: conversationId,
      project_id: projectId,
      project_key: projectKey,
      // The resource-id route is deliberately used as the launcher. ChatGPT
      // currently redirects it to the user-facing `/project` route, including
      // any cosmetic Project slug.
      project_launch_url: projectKey,
    };
  } catch (_) {
    return null;
  }
}

export function isProjectConversation(rawUrl) {
  return Boolean(chatGptConversationInfo(rawUrl)?.project_id);
}

function compactWorkspaceLabels(bindings) {
  const labels = [];
  for (const b of bindings || []) {
    const value = String(b?.workspace_label || b?.workspace_id || "").trim();
    if (value && !labels.includes(value)) labels.push(value);
  }
  return labels.slice(0, 12);
}

/** Build the user message sent to the CURRENT conversation to create a handoff. */
export function buildHandoffRequest({ transferId, bindings = [], template = null } = {}) {
  const id = String(transferId || "").trim();
  if (!id) throw new Error("transferId is required");
  const workspaces = compactWorkspaceLabels(bindings);
  const workspaceLine = workspaces.length ? workspaces.join(", ") : "(none)";
  if (template) {
    return String(template)
      .replaceAll("{id}", id)
      .replaceAll("{workspace_line}", workspaceLine);
  }
  return [
    "这是由 Herdr 浏览器扩展发起的对话接力请求。",
    "本轮不要继续实现，不要调用工具，也不要追问。",
    "请总结当前工作状态，让同一 Project 中的新 ChatGPT 对话可以继续推进，同时避免重复已经完成的工作。",
    "只能使用本对话已经确认的事实。不要虚构当前 runtime/Git 状态；新对话开始 mutation 前必须重新检查实时状态。",
    `已绑定 Herdr workspace：${workspaceLine}。`,
    "必须包含：当前目标、已完成工作、重要决定、进行中/未完成工作、已知的准确文件/分支/提交/runtime 事实、安全约束，以及建议下一步。",
    "保持精炼但可直接继续执行。保留重要的字面标识符、路径、命令、版本、URL 和任务 ID。",
    "只返回下面标记之间的一份 handoff packet，标记前后不要添加其他文字。",
    "",
    `${HANDOFF_BEGIN} id=${id}>>>`,
    "# 项目接力",
    "<!-- 在这里写入精炼的 handoff -->",
    HANDOFF_END,
  ].join("\n");
}

/** Extract and validate one handoff packet from the assistant reply. */
export function extractHandoffPacket(text, transferId) {
  const body = String(text || "");
  const id = String(transferId || "").trim();
  if (!id) return null;
  const marker = `${HANDOFF_BEGIN} id=${id}>>>`;
  const start = body.indexOf(marker);
  if (start < 0) return null;
  const end = body.indexOf(HANDOFF_END, start + marker.length);
  if (end < 0) return null;
  const packet = body.slice(start, end + HANDOFF_END.length).trim();
  if (packet.length < marker.length + HANDOFF_END.length + 20) return null;
  if (packet.length > MAX_HANDOFF_PACKET_CHARS) return null;
  return packet;
}

/**
 * Classify an assistant body observed while a handoff summary is pending.
 * ChatGPT can report the previous settled assistant body again immediately
 * after the handoff user message is submitted but before the new turn starts.
 */
export function classifyHandoffAssistantReply({
  text,
  transferId,
  sourceAssistantFingerprint = null,
  currentAssistantFingerprint = null,
} = {}) {
  const body = String(text || "").trim();
  const packet = extractHandoffPacket(body, transferId);
  if (packet) return { kind: "packet", packet };
  if (!body) return { kind: "pending", packet: null };
  if (
    sourceAssistantFingerprint
    && currentAssistantFingerprint
    && sourceAssistantFingerprint === currentAssistantFingerprint
  ) {
    return { kind: "stale_source", packet: null };
  }
  return { kind: "invalid", packet: null };
}

/** Build the first user message in the NEW conversation. */
export function buildHandoffSeed({ transferId, packet, template = null } = {}) {
  const id = String(transferId || "").trim();
  const handoff = String(packet || "").trim();
  if (!id || !handoff) throw new Error("transferId and packet are required");
  if (handoff.length > MAX_HANDOFF_PACKET_CHARS) throw new Error("handoff packet too large");
  if (template) {
    return String(template)
      .replaceAll("{id}", id)
      .replaceAll("{packet}", handoff);
  }
  return [
    "继续同一个项目。以下内容由 Herdr 浏览器扩展从上一段对话接力而来。",
    "把 handoff 作为工作状态上下文，不要把它视为实时状态仍未变化的证明。",
    "开始任何 mutation 前，重新检查相关的 Herdr/runtime/Git 状态。不要因为 handoff 里提到过，就重复执行已经完成的工作。",
    "",
    `${HANDOFF_SEED_PREFIX} id=${id}]`,
    handoff,
    "[/HERDR_CONTINUITY_TRANSFER]",
  ].join("\n");
}

export function handoffSeedContainsTransfer(text, transferId) {
  const id = String(transferId || "").trim();
  if (!id) return false;
  return String(text || "").includes(`${HANDOFF_SEED_PREFIX} id=${id}]`);
}

export function handoffStatusIsActive(status) {
  return [
    "summary_requested",
    "summary_ready",
    "target_opening",
    "seed_submitting",
    "seed_uncertain",
  ].includes(String(status || ""));
}

export function newContinuityId(now = Date.now(), random = Math.random()) {
  const r = Math.floor(Math.max(0, Math.min(0.999999999, Number(random) || 0)) * 0x100000000)
    .toString(36)
    .padStart(7, "0");
  return `hc:${Number(now).toString(36)}:${r}`;
}

export function newTransferId(now = Date.now(), random = Math.random()) {
  const r = Math.floor(Math.max(0, Math.min(0.999999999, Number(random) || 0)) * 0x100000000)
    .toString(36)
    .padStart(7, "0");
  return `ht:${Number(now).toString(36)}:${r}`;
}
