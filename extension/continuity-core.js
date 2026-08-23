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
export function buildHandoffRequest({ transferId, bindings = [] } = {}) {
  const id = String(transferId || "").trim();
  if (!id) throw new Error("transferId is required");
  const workspaces = compactWorkspaceLabels(bindings);
  const workspaceLine = workspaces.length ? workspaces.join(", ") : "(none)";
  return [
    "This is a conversation rollover request from the Herdr browser extension.",
    "Do not continue implementation, do not call tools, and do not ask follow-up questions in this turn.",
    "Summarize the CURRENT working state so a fresh ChatGPT conversation in the same Project can continue without replaying completed work.",
    "Use only facts already established in this conversation. Do not invent current runtime/Git state; the next conversation must verify live state before mutations.",
    `Bound Herdr workspaces: ${workspaceLine}.`,
    "Include: current objective, completed work, important decisions, active/incomplete work, exact files/branches/commits/runtime facts when known, constraints/safety rules, and the recommended next actions.",
    "Keep it compact but operationally complete. Preserve literal identifiers, paths, commands, versions, URLs, and task IDs that matter.",
    "Return exactly one handoff packet between the markers below. No prose before or after the markers.",
    "",
    `${HANDOFF_BEGIN} id=${id}>>>`,
    "# Project handoff",
    "<!-- write the compact handoff here -->",
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

/** Build the first user message in the NEW conversation. */
export function buildHandoffSeed({ transferId, packet } = {}) {
  const id = String(transferId || "").trim();
  const handoff = String(packet || "").trim();
  if (!id || !handoff) throw new Error("transferId and packet are required");
  if (handoff.length > MAX_HANDOFF_PACKET_CHARS) throw new Error("handoff packet too large");
  return [
    `Continue the same project from the compact handoff below. This message was transferred by the Herdr browser extension.`,
    "Treat the handoff as working-state context, not as proof that live state is unchanged.",
    "Before any mutation, verify the relevant current Herdr/runtime/Git state. Do not rerun completed work merely because it is mentioned in the handoff.",
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
