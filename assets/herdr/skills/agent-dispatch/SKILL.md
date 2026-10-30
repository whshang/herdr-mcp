---
name: agent-dispatch
description: Advise, submit, and verify useful independent coding-agent work from live worker capability/status facts while preserving Web-planner choice, explicit targets, and quality requirements.
---

# Agent Dispatch

Own: `herdr_prompt` and `herdr_call` `herdr_mcp.agent.task.dispatch` (the WebChat binding path). Combine this policy with live facts from `herdr_inspect`/`herdr_since`; never encode a permanent agent/model ranking or infer a worker's role from its name/kind.

Call shape: target one explicit live Agent name, pane identity, or role selector and use one stable `idempotency_key` for the intended prompt submission. When the task needs a **new** agent, add the optional `spawn` object to the same `herdr_mcp.agent.task.dispatch` call (`spawn: {kind, workspace_id | cwd, name?, tab_label?, split_from?, direction?, startup_timeout_ms?}`): one call creates the pane, starts the agent, waits for interactive readiness and prompts it, so there is no separate `pane.split` / `agent.start` step and only one confirmation. `spawn` and `target` are mutually exclusive and `spawn` requires `idempotency_key`; replaying that key never creates a second pane, tab or agent. A spawn default creates a new tab (`tab.create`), because a horizontal split of a narrow tab is refused (`pane_split_too_narrow`); pass `spawn.split_from` only when that parent pane is wide enough. Failures report `failed_step` (`validate`, `pane_create`, `agent_start`, `agent_ready`, `prompt`), the native `code`, `delivery_state: not_delivered` when the prompt was never sent, and the created pane/agent for reclaim. A WebChat caller whose task should bind to its conversation dispatches through `herdr_call` with `method: "herdr_mcp.agent.task.dispatch"` and passes the exact current user turn, verbatim (trimmed, never paraphrased or summarized), as `current_user_message` so the runtime can bind the task to that conversation's browser session (`parent_session_ref`). `herdr_prompt` never performs source-turn binding: it ignores `current_user_message` and only reuses an existing caller-session mapping, so use it only when binding is not needed. Binding is evidence-based and a `parent_session: {bound:false}` fact never fails the dispatch; when the result reports `bound:false`, tell the user `parent_session.reason` and the non-secret `parent_session.diagnostic` prefixes instead of retrying or redispatching. Normal orchestration is asynchronous; retain the returned task/dispatch identity instead of blocking on a global Agent state. After timeout or transport failure, retry only when delivery evidence says `not_delivered` or live state proves the prompt did not apply.

Delegate non-trivial implementation per `development-orchestration`'s main-brain rule; delegation never removes the planner's own fs/exec capability.

## Selection

1. Delegation stays a planner choice, not a side effect of idle workers or candidate availability.
2. Filter live candidates by auto-dispatch policy, project/cwd, status, lane ownership, mutation conflict, and capabilities the task actually requires.
3. Expose compatible candidates with known quality/cost/latency evidence to the Web planner. Unknown capability fields stay unknown; candidate availability never requires dispatch.
4. Submit one bounded task card: objective, role declaration (executor — implement and report, no dispatch/merge/push unless the card says so; or supervisor — the task's single designated dispatcher, re-dispatching with `--parent` = its own pane, supervising, reviewing, integrating, and reporting once, still merging/pushing only if the card says so), worktree/branch, file scope, validation commands, report boundary. Give the executor only the context the card needs — not the chat transcript, not planner-only dispatch rules — and keep the card well under 12 KB. Then verify delivery through prompt evidence plus live state.
5. Record a progress checkpoint for delegated work. On the next relevant planner turn and before integration, use `herdr_since` plus the lane's Git/output evidence to confirm progress; when evidence shows drift or a stall, tighten the prompt, stop the lane, or reassign it before starting another worker.

A user-specified agent/model/pane target has priority and is never silently replaced. A busy preferred worker may fall back only to a reliably equivalent compatible worker; do not silently lower capability or quality.

Project or user instructions (`AGENTS.md`, `HANDOFF.md`) may declare a role mapping such as `Coder: pi -> omp; Reviewer: grok -> droid`. Honor that order for that project, still subject to live availability, capability, and occupancy; the mapping is project policy, not a ranking this Skill may encode. Review lane and advisor triggers are owned by `development-orchestration`.

Generic same-project reasoning may consider an idle allowed worker even when optional model/edit/vision traits are unknown. A task that actually requires one of those traits must fail closed until the capability is verified.

## Boundaries and evidence

Reject blocked workers, project mismatch, conflicting mutation ownership, destructive production/runtime mutation, and unscoped middle-manager chains. A worker that would plan or manage other workers is admissible only when the Web planner designates it as that task's single supervisor; its own workers never re-dispatch. Do not invent work because a worker is idle.

Keep bounded `DispatchAdvice` evidence: task profile, direct deterministic option, compatible candidates, relevant rejection reasons, optional parallelism opportunity, ownership scope, and validation boundary. A selected target exists only after the Web planner chooses one. Uncertain submission is observed before any retry.

If native workers are unavailable or unsuitable, a configured headless external harness may be used only as a bounded fallback for a self-contained task. Give it an explicit checkpoint and verify Git/files/tests afterward; process exit alone is not completion evidence. Interactive fallback remains human-operated.

`agent_not_found`, `unknown_agent`, or an unbound pane is a **pre-dispatch condition**, not a human boundary. When prompt evidence says `delivery_state=not_delivered` and `requires_human=false`, do not stop. Re-read `herdr_inspect`, use deterministic `startable_candidates` / `agent_lifecycle`, and choose a compatible Agent kind. Reuse a pane only when this task already owns and has verified that free shell pane; otherwise re-dispatch once with `spawn: {kind, workspace_id | cwd}` under a new idempotency key, which creates a task-owned pane, starts the Agent, waits for readiness and prompts it in one call. Only when the runtime does not accept `spawn` fall back to creating a task-owned pane with `pane.split`, starting the Agent with `agent.start`, waiting for readiness, then re-prompting with the same idempotency key. `agent:null` proves only that no Agent is bound; it never proves planner ownership. Reclaim only panes this task created, and only after the worker is terminal, delivery is certain, and its output/Git evidence is captured.

When `startable_candidates.evidence_gap.present=true`, an installed Agent exists but a required capability is not yet verified. Keep the capability gate fail-closed, run the advertised capability refresh, and re-plan; never rewrite “unverified” as “Agent unavailable” or “Herdr unavailable”.

## External host outcome

A response with no Herdr execution identity or result fields is not evidence that the workstation, child process, filesystem, or remote service executed anything. When Herdr result fields are present, use those fields to describe the local operation. Host policy is external to Agent Dispatch and does not change worker selection or task ownership.

Optional `inject_task_identity: true` appends one `[herdr-task task_id=… dispatch_id=…]` line and a completion hint to the worker prompt. The default is false (prompt unchanged); injection does not change target/text fingerprinting, and replay never submits again. Dispatch results, task status and inbox items expose separate `task_id` and `dispatch_id` fields; callers must not assume equality even when values currently match. The completion hint uses the trusted local worker report CLI below.

If you already obtained an exact browser session resource ref, pass `parent_session_ref`. It takes precedence over `current_user_message` (which is still stripped from the prompt path). WebChat binding requires a registered session, a supported matching provider, current provider generation, a covering endpoint/provider control grant, an available account and an already-existing durable mapping for this principal/provider/opaque caller session that equals the explicit ref. The explicit path never creates a caller-session mapping. If no mapping exists, it leaves the task unstamped with reason `explicit_parent_session_caller_session_unmapped`; omit the explicit ref and use the existing source-turn path for first-time binding. Any miss leaves the task unstamped with `parent_session: {bound:false, reason:"explicit_parent_session_<reason>"}` and never falls back to turn text or the caller mapping. Trusted local IPC without WebChat identity accepts only a registered session resource. Without the explicit ref, existing source-turn/mapping behavior is unchanged. A replay reports the stored stamp and does not bind again.

Local CLI: `herdr-mcp agent dispatch <target> <text> --parent <target> --idempotency-key <key> [--inject-task-identity] [--parent-session-ref <ref>]`; for a new agent use `herdr-mcp agent dispatch --spawn-kind <kind> [--spawn-workspace <id-or-label>] [--spawn-cwd <path>] [--spawn-name <name>] <text> --idempotency-key <key> [--parent <target>]`.

## Worker reports and wait checkpoints

Workers use `herdr-mcp agent report --task-id ID --dispatch-id ID --kind worker_done [--summary TEXT]` exactly once, and `--kind heartbeat` periodically during long tasks. The private method is `herdr_mcp.agent.task.report` with the same snake_case parameters. It requires trusted local IPC and rejects WebChat callers; no public MCP tool is added. Both ids must match one durable task; a duplicate completion returns `worker_done_already_reported` without changing state. A late completion records the report fact while preserving the settled lifecycle and parent notification. Summaries have control characters stripped and are bounded to 2 KiB of UTF-8.

Status and all inbox projections expose derived `liveness: active|stalled`. A non-terminal task becomes stalled after 20 minutes without heartbeat/report/progress; dispatch may override this with `stall_after_ms` (`--stall-after-ms`), bounded to 1–120 minutes. A heartbeat updates timestamps only. Stalled tasks stay non-terminal and are never automatically redispatched.

Dispatch `wait` (`--wait --timeout-ms N`) timing out returns `ok:true`, `wait.completed:false`, `wait.outcome:"checkpoint"`, and the current lifecycle/liveness snapshot, with no failure field. Re-check task status/inbox using the existing identity. Delivery failure remains a failure.

## Explicit role targets and concurrency

`target: "@idle"` requests any idle agent pane. `@codex`, `@grok`, `@droid`, `@claude`, `@pi` (or another live kind), optionally suffixed `:idle`, filter by kind and still require idle status. Selection follows live snapshot order, excluding working/blocked/unknown panes and the `parent_target` caller pane, and preserves delegation guards. Pass the caller's own agent/pane as `parent_target`. Optional `workspace_id` confines role resolution. A role explicitly requests this candidate set; exact names/panes never fall back to another worker. `@all`, broadcast, and other selector syntax are rejected with `role_target_unsupported`; no idle candidate returns `role_target_unavailable` with `delivery_state: "not_delivered"`. No agents are spawned.

Role results return non-secret `candidates`, `resolved_target`, and `requested_target`; the durable task stores the selector as `requested_target` and the concrete pane as `target`. Replaying the same idempotency key returns the original resolution without submitting or resolving again. A rejected keyed request also replays its original rejection; use a new key for a new attempt after availability changes.

Optional `max_concurrent` (integer 1–16, role targets only) rejects with `concurrency_limit_reached` and `delivery_state: "not_delivered"` when non-terminal tasks for this exact selector reach the cap. Scope is the bound `parent_session_ref`, otherwise `parent_target`, otherwise caller-less global work. Open decision gates count as non-terminal. Unstamped work never shares a browser session's scope. Admission and task registration are serialized within the runtime to prevent concurrent requests exceeding the cap; unrelated exact-target behavior is unchanged.

CLI: `herdr-mcp agent dispatch --target @codex "implement the task" --parent w1:p1 --idempotency-key task-1 --max-concurrent 2 [--workspace-id w1]`. The positional `<target> <text>` form remains supported.
