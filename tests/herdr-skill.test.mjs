import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import {
  herdrSkillPointer,
  fetchHerdrSkill,
  HERDR_MCP_SKILL_BUNDLED,
} from "../dist/herdr-skill.js";

function workstationOfflineBackoffSeconds() {
  const source = readFileSync(new URL("../edge/cloudflare/src/errors.ts", import.meta.url), "utf8");
  const match = source.match(/WORKSTATION_OFFLINE_RETRY_BACKOFF_MS\s*=\s*\[([^\]]+)\]/);
  assert.ok(match, "Edge retry backoff SSOT must be readable");
  const values = [...match[1].matchAll(/\b([\d_]+)\b/g)].map((entry) => Number(entry[1].replaceAll("_", "")));
  assert.ok(values.length > 0, "Edge retry backoff SSOT must contain at least one value");
  assert.ok(values.every((value) => value % 1000 === 0), "Skill recovery prose is expressed in whole seconds");
  return values.map((value) => value / 1000);
}

test("herdrSkillPointer exposes project policy, native reference and self-update entrypoint", () => {
  const p = herdrSkillPointer();
  assert.equal(p.tool, "herdr_skill");
  assert.match(p.project_upstream, /whshang\.github\.io\/herdr-mcp\/herdr-mcp-SKILL\.md/);
  assert.equal(p.project_bundled, HERDR_MCP_SKILL_BUNDLED);
  assert.equal(p.native_reference, "local:herdr --skill");
  assert.equal(p.self_update, "herdr-self-update");
  assert.match(p.hint, /Remote-planner policy first/);
});

test("fetchHerdrSkill offline mode returns bundled project policy plus live runtime context", async () => {
  const prev = process.env.HERDR_SKILL_NETWORK;
  process.env.HERDR_SKILL_NETWORK = "0";
  try {
    const r = await fetchHerdrSkill({ refresh: true, includeNativeReference: false });
    assert.equal(r.ok, true);
    assert.equal(r.project_skill.origin, "bundled");
    assert.equal(r.project_skill.source, HERDR_MCP_SKILL_BUNDLED);
    assert.match(r.content, /# herdr-mcp remote planner skill/);
    assert.match(r.content, /Direct workstation operations first/);
    assert.match(r.content, /AGENTS\.md.*CLAUDE\.md.*README\.md/s);
    assert.match(r.content, /including read-only analysis/);
    assert.match(r.content, /\.agents\/skills\/\*\/SKILL\.md/);
    assert.doesNotMatch(r.content, /\.claude\/skills\/\*\/SKILL\.md/);
    assert.match(r.content, /Prefer project-scoped skills over same-name user-scoped skills/);
    assert.match(r.content, /herdr-self-update apply/);
    assert.match(r.content, /Do not treat exit code 0 alone as completion evidence/);
    assert.match(r.content, /Control-plane outage recovery/);
    assert.match(r.content, /RunAtLoad=true.*KeepAlive=true/s);
    assert.match(r.content, /dev\.herdr-mcp\.health-watchdog/);
    assert.match(r.content, /historical `dev\.herdr-mcp\.watchdog` identity/);
    assert.match(r.content, /health-watchdog\.\*/);
    const backoffSeconds = workstationOfflineBackoffSeconds();
    for (const seconds of backoffSeconds) {
      assert.match(r.content, new RegExp(`\\b${seconds} seconds\\b`));
    }
    const recoveryWindowSeconds = backoffSeconds.reduce((sum, seconds) => sum + seconds, 0);
    assert.match(r.content, new RegExp(`roughly.*${recoveryWindowSeconds} seconds`, "s"));
    assert.match(r.content, /read-only.*reconnect attempts/s);
    assert.match(r.content, /not a human-escalation threshold/);
    assert.match(r.content, /requires_human=false.*no-escalation signal/s);
    assert.match(r.content, /delivery_uncertain.*automatically inspect.*request\/resource evidence/s);
    assert.match(r.content, /Do not ask the operator to restart Herdr merely because the probe window elapsed/);
    assert.match(r.content, /macOS permission\/TCC prompt.*browser\/account\/OAuth.*irreversible action.*automatic recovery is genuinely exhausted/s);
    assert.match(r.content, /agent_status_wait_timeout.*not.*offline/s);
    assert.match(r.content, /boot_id.*herdr_since\(cursor=0\)/s);
    assert.match(r.content, /never blindly resend it/s);
    assert.match(r.content, /Live herdr-mcp runtime context/);
    assert.match(r.content, /Latency-aware tool scheduling/);
    assert.match(r.content, /dependency-aware \*\*wave\*\*/);
    assert.match(r.content, /host-side rejection that contains none of Herdr's execution identity or result fields/s);
    assert.match(r.content, /provides no workstation or child-process execution result/s);
    assert.match(r.content, /When Herdr result fields are present, their execution and delivery evidence is authoritative/s);
    assert.match(r.content, /Host-side policy remains external to Herdr and is not altered by this planner guide/);
    assert.match(r.content, /delegation_allowed=false.*no compatible \*\*live\*\* worker.*not by itself a stop condition/s);
    assert.match(r.content, /agent_lifecycle\.action=start_agent_then_dispatch.*agent\.start/s);
    assert.match(r.content, /Never treat `agent:null`.*as proof that Herdr\/tooling is unavailable/s);
    assert.match(r.content, /If browser control is unavailable, use the already-prepared Copy Prompt/);
    assert.match(r.content, /If automatic delivery produces no Herdr execution or result fields.*use the prepared manual path/s);
    assert.match(r.content, /If Herdr reports uncertain delivery, do not replay the mutation automatically/);
    assert.doesNotMatch(r.content, /One identical retry is allowed/);
    assert.doesNotMatch(r.content, /existing local Agent.*execution fallback/s);
    assert.doesNotMatch(r.content, /pre-delivery safety rejection.*retry the original create arguments/s);
    assert.match(r.content, /one logical intent, one authority boundary, and at most one mutation boundary/);
    assert.match(r.content, /internal Herdr\/Edge batching, caching and coalescing remain encouraged/i);
    assert.match(r.content, /concatenating unrelated host, file, database, network, or service actions into one freeform `herdr_exec`/);
    assert.match(r.content, /must stay separate public calls even when that costs an extra round trip/);
    assert.match(r.content, /does not by itself make any tool "absolutely safe".*not permission to weaken authorization/s);
    assert.match(r.content, /herdr_git status.*diff.*log.*herdr_exec.*herdr_fs_grep.*compacted/s);
    assert.match(r.content, /counts.*compacted.*summarized `output`/s);
    assert.match(r.content, /Long build\/test\/process work belongs in `herdr_exec_start`, not the canonical utility pane or a blocking `herdr_exec`/);
    assert.match(r.content, /herdr_mcp\.exec\.wait/);
    assert.match(r.content, /GitHub Actions artifact downloads.*`herdr_exec_start`/s);
    assert.match(r.content, /destination-file growth.*never start a duplicate transfer/s);
    assert.match(r.content, /herdr_exec_read\(offset=next_offset\)/);
    assert.match(r.content, /prefer `herdr_exec_start` -> `herdr_exec_read` \(delta\) over a blocking `herdr_exec`/);
    assert.match(r.content, /phase=started/);
    assert.match(r.content, /phase=completed/);
    assert.match(r.content, /progress.*bytes_read.*bytes_total.*elapsed_ms/s);
    assert.match(r.content, /completion resource sweep/);
    assert.match(r.content, /Do not wait for the user to notice accumulated panes/);
    assert.match(r.content, /settled Agent.*does not need to remain open.*preserve task history/s);
    assert.match(r.content, /Ordinary roots run as durable native sessions and do not require a visible pane/);
    assert.match(r.content, /macOS privacy-protected roots \(Documents\/Desktop\/Downloads\).*canonical `herdr-mcp:utility` pane/s);
    assert.match(r.content, /current public schema advertises `herdr_exec\.steps`.*transparent structured `program` \+ `args`/s);
    assert.match(r.content, /Use freeform `herdr_exec\.command` only when actual shell semantics/s);
    assert.match(r.content, /Do not concatenate several otherwise independent process invocations with `&&`, `;`/s);
    assert.match(r.content, /Use `herdr_exec` directly for bounded process work in the selected workspace\/project/);
    assert.match(r.content, /structured `herdr_exec\.steps`.*batch only already-known same-boundary process invocations/s);
    assert.match(r.content, /Do not use `pane\.split` or `herdr_exec_start` merely to get another shell/);
    assert.match(r.content, /macOS privacy-protected roots \(Documents\/Desktop\/Downloads\).*canonical `herdr-mcp:utility` pane/s);
    assert.match(r.content, /Bounded SSH maintenance\/recovery.*long remote work uses one durable session.*same `session_id`/s);
    assert.match(r.content, /Automation Client/);
    assert.match(r.content, /grant_type=client_credentials/);
    assert.match(r.content, /maximum one hour, no refresh token/);
    assert.match(r.content, /ordinary MCP principals bound to exactly one enrolled device, not fleet administrators/);
    assert.match(r.content, /Automation administration remains an enrolled-device\/operator CLI\/REST action/);
    assert.doesNotMatch(r.content, /herdr_mcp\.automation\.(?:list|revoke)/);
    assert.match(r.content, /127\.0\.0\.1:8772\/mcp.*still requires its local bearer/s);
    assert.match(r.content, /trusted Unix-IPC listener is deliberately tokenless/);
    assert.match(r.content, /browser never receives or stores `HERDR_MCP_TOKEN`/);
    assert.equal(r.runtime.contract_profile, process.env.HERDR_MCP_CONTRACT_PROFILE || "current");
    assert.equal(r.runtime.build_commit, process.env.HERDR_MCP_BUILD_COMMIT || null);
    assert.equal(r.runtime.active_runtime.source_commit, process.env.HERDR_MCP_BUILD_COMMIT || null);
    assert.equal(r.runtime.active_runtime.truth_source, "active_binary+runtime_generation_manager");
    assert.equal(typeof r.runtime.worker_fallbacks, "object");
    assert.equal(r.runtime.worker_fallbacks.dsh_headless.invocation, "herdr_exec_start -> dsh --profile headless <task>");
    assert.equal(r.runtime.worker_fallbacks.dsh_tui.role, "human-interactive fallback");
    assert.equal(r.native_reference, undefined);
    assert.ok(r.bytes > 3000);
  } finally {
    if (prev === undefined) delete process.env.HERDR_SKILL_NETWORK;
    else process.env.HERDR_SKILL_NETWORK = prev;
  }
});

test("agent-dispatch skill keeps host policy external to worker selection", () => {
  const content = readFileSync(
    new URL("../assets/herdr/skills/agent-dispatch/SKILL.md", import.meta.url),
    "utf8",
  );
  assert.match(content, /External host outcome/);
  assert.match(content, /no Herdr execution identity or result fields/);
  assert.match(content, /Host policy is external to Agent Dispatch/);
  assert.doesNotMatch(content, /one bounded, identical retry/);
  assert.doesNotMatch(content, /existing compatible local Agent/);
  assert.doesNotMatch(content, /fallback chain stops/);
});

test("fetchHerdrSkill appends release-matched native Herdr reference with remote-scope warning", async () => {
  const prev = process.env.HERDR_SKILL_NETWORK;
  process.env.HERDR_SKILL_NETWORK = "0";
  try {
    const r = await fetchHerdrSkill({ refresh: true, includeNativeReference: true });
    assert.equal(r.ok, true);
    assert.ok(r.native_reference);
    assert.match(r.native_reference.source, /^(local:herdr --skill|bundled:assets\/herdr-agent-SKILL\.md)$/);
    assert.match(r.content, /Appendix: release-matched native Herdr reference/);
    assert.match(r.content, /HERDR_ENV=1.*does \*\*not\*\* override/s);
    assert.match(r.content, /engineering-robustness/);
    assert.match(r.content, /silent-wrongness/);
    assert.match(r.content, /state as separate planes/);
    assert.match(r.content, /continuity\.search/);
    assert.match(r.content, /confirmation_required/);
    assert.match(r.content, /Do not ask the user to provide a Herdr continuity ID before attempting safe discovery/);
    assert.match(r.content, /Never.*newest.*textually most similar/s);
    assert.match(r.content, /name: herdr/);
  } finally {
    if (prev === undefined) delete process.env.HERDR_SKILL_NETWORK;
    else process.env.HERDR_SKILL_NETWORK = prev;
  }
});
