import test from "node:test";
import assert from "node:assert/strict";
import { filterWebChatGrantsForRuntime } from "../dist/webchat-grant-capability.js";

const STAR = { endpoint_ref: "bep_a", provider: "*", account_ref: "*" };
const GROK = { endpoint_ref: "bep_a", provider: "grok", account_ref: "*" };

test("user on an old runtime | Given a derived wildcard (*) grant and an explicit grok grant | When the runtime does not advertise webchat_control.any_provider | Then only the explicit per-provider grant is forwarded", () => {
  const out = filterWebChatGrantsForRuntime({ webchat_control_grants: [STAR, GROK], page_assist_grants: [{ endpoint_ref: "bep_a" }] }, ["relay.request"]);
  assert.deepEqual(out, { webchat_control_grants: [GROK], page_assist_grants: [{ endpoint_ref: "bep_a" }] });
});

test("user on an old runtime | Given only a derived wildcard (*) grant | When the runtime advertises no capabilities | Then no WebChat control grant is forwarded", () => {
  const out = filterWebChatGrantsForRuntime({ webchat_control_grants: [STAR] }, undefined);
  assert.deepEqual(out, {});
});

test("user on a new runtime | Given a derived wildcard (*) grant | When the runtime advertises webchat_control.any_provider | Then the wildcard grant is forwarded unchanged", () => {
  const trace = { webchat_control_grants: [STAR, GROK] };
  assert.equal(filterWebChatGrantsForRuntime(trace, ["relay.request", "webchat_control.any_provider"]), trace);
});
