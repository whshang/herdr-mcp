// Browser Control Plane Phase A explicit target pinning.
// The revision is extension-local until Rust exposes an authoritative revision.

function targetFingerprint(pane) {
  if (!pane?.pane_id) return null;
  const agent = pane.agent || null;
  return [
    pane.workspace_id || "",
    pane.pane_id,
    agent?.name || "terminal",
    agent?.kind || "terminal",
    agent?.started_at || "",
  ].join("|");
}

export function createPinnedTarget(pane, revision = null, now = () => new Date()) {
  const targetRevision = revision || targetFingerprint(pane);
  return {
    workspace_id: pane?.workspace_id || null,
    pane_id: pane?.pane_id || null,
    agent: pane?.agent || null,
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
    status: pane.status || target.status || "unknown",
    stale: revisionChanged,
    stale_reason: revisionChanged ? "target_revision_changed" : null,
  };
}

export function focusChangedDoesNotRetarget(target) {
  return target;
}

export const targetPinInternals = Object.freeze({ targetFingerprint });
