// Browser Control Plane explicit target pinning.
// Prefer the runtime-authoritative target revision; keep a local fingerprint only for legacy snapshots.

function targetFingerprint(pane) {
  if (!pane?.pane_id) return null;
  if (pane.target_revision) return String(pane.target_revision);
  const agent = pane.agent || null;
  const session = pane.agent_session || null;
  return [
    pane.workspace_id || "",
    pane.pane_id,
    pane.revision ?? "",
    agent?.name || "terminal",
    agent?.kind || "terminal",
    agent?.started_at || "",
    session?.source || "",
    session?.kind || "",
    session?.value || "",
  ].join("|");
}

export function createPinnedTarget(pane, revision = null, now = () => new Date()) {
  const targetRevision = revision || targetFingerprint(pane);
  return {
    workspace_id: pane?.workspace_id || null,
    pane_id: pane?.pane_id || null,
    agent: pane?.agent || null,
    agent_session: pane?.agent_session || null,
    control_capabilities: pane?.control_capabilities || null,
    target_revision: targetRevision,
    status: pane?.status || "unknown",
    stale: !pane?.pane_id,
    stale_reason: pane?.pane_id ? null : "pane_missing",
    pinned_at: now().toISOString(),
  };
}

export function revalidatePinnedTarget(target, panes = []) {
  if (!target?.pane_id) return null;
  const pane = panes.find((item) => item.pane_id === target.pane_id);
  if (!pane) {
    return { ...target, stale: true, stale_reason: "pane_removed" };
  }
  const currentRevision = targetFingerprint(pane);
  const revisionChanged = Boolean(target.target_revision && currentRevision && target.target_revision !== currentRevision);
  return {
    ...target,
    workspace_id: pane.workspace_id || target.workspace_id || null,
    agent: pane.agent || null,
    agent_session: pane.agent_session || null,
    control_capabilities: pane.control_capabilities || null,
    target_revision: currentRevision || target.target_revision || null,
    status: pane.status || target.status || "unknown",
    stale: revisionChanged,
    stale_reason: revisionChanged ? "target_revision_changed" : null,
  };
}

export function focusChangedDoesNotRetarget(target) {
  return target;
}

export const targetPinInternals = Object.freeze({ targetFingerprint });
