/**
 * Prompt / herdr_call error + delivery observation helpers.
 * Keeps "submission" vs "status wait" vs "true transport" distinguishable for clients.
 */

export type ObservationChanged = true | false | "unknown";

export function isAgentStatusWaitTimeout(message: string): boolean {
  const m = String(message || "");
  return /timed out waiting for agent status/i.test(m)
    || /waiting for agent status/i.test(m);
}

/** True socket/connect failures — not daemon business logic timeouts. */
export function isTrueTransportFailure(code: string, message: string): boolean {
  if (isAgentStatusWaitTimeout(message)) return false;
  return ["socket_error", "connection_refused", "socket_missing", "parse_error"].includes(code)
    || (code === "timeout" && /connect /i.test(message));
}

/**
 * Build state_observation for herdr_prompt success (and compatible state_changed).
 * Without wait, an unchanged snapshot is "unknown" (seq may lag), not false.
 */
export function buildStateObservation(args: {
  before: { agent_status: string | null; state_change_seq: number | null } | null;
  after: { agent_status: string | null; state_change_seq: number | null } | null;
  waited: boolean;
}): { state_observation: { changed: ObservationChanged; fresh: boolean }; state_changed: boolean } {
  const { before, after, waited } = args;
  const boolChanged = !!before && !!after
    && (before.state_change_seq !== after.state_change_seq || before.agent_status !== after.agent_status);
  let changed: ObservationChanged;
  if (!after) changed = "unknown";
  else if (boolChanged) changed = true;
  else if (waited) changed = false;
  else changed = "unknown";
  return {
    state_observation: { changed, fresh: !!after && (waited || boolChanged) },
    state_changed: boolChanged,
  };
}
