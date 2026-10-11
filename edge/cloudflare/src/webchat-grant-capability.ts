/**
 * Runtimes advertise `webchat_control.any_provider` in the link hello when they
 * accept provider "*" WebChat control grants. Older runtimes reject the whole
 * trusted grant context if they see "*", so strip those grants for them and
 * keep only explicit per-provider grants.
 */
export const WEBCHAT_CONTROL_ANY_PROVIDER_CAPABILITY = "webchat_control.any_provider";

export function filterWebChatGrantsForRuntime(
  trace: Record<string, unknown> | undefined,
  capabilities: readonly string[] | undefined,
): Record<string, unknown> | undefined {
  if (!trace || !Array.isArray(trace.webchat_control_grants)) return trace;
  if (capabilities?.includes(WEBCHAT_CONTROL_ANY_PROVIDER_CAPABILITY)) return trace;
  const grants = (trace.webchat_control_grants as unknown[]).filter((grant) =>
    !(grant && typeof grant === "object" && (grant as { provider?: unknown }).provider === "*"));
  if (grants.length === (trace.webchat_control_grants as unknown[]).length) return trace;
  const { webchat_control_grants: _dropped, ...rest } = trace;
  return grants.length > 0 ? { ...rest, webchat_control_grants: grants } : rest;
}
