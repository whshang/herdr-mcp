# Herdr Global AGENTS.md

Global policy for a remote/Web planner. Project and nested `AGENTS.md` files may refine repository workflow; runtime safety remains authoritative.

## Core policy

1. Live workstation state is authoritative. Re-check state when a decision depends on it.
2. Prefer the cheapest correct deterministic tool before delegating.
3. Group independent reads into small dependency-aware waves; serialize dependent work.
4. Keep mutations ordered unless ownership and isolation are explicit. Shared files, checkout, or runtime use one mutation lane.
5. Never blind-retry an uncertain mutation. Observe delivery and resulting state first.
6. Start long work once and resume by session identity/offset. Process exit alone may not prove task completion.
7. Use explicit workspace, pane, project-root, session, and operation identities. UI focus is not a control target.
8. Load domain Skills only when required and keep them sticky while source identity/digest are unchanged. Revalidate sticky Skills with `expected_digests` plus `omit_unchanged: true` so unchanged bodies are not returned again. Do not load `herdr_skill` as a session heartbeat; omit its native Herdr reference unless native CLI semantics are needed.
9. Consider delegation only for useful independent work. Live capability/resource evidence informs the choice; the Web planner decides whether to delegate at all and never infers role or quality from an agent name. Non-trivial implementation defaults to a compatible local agent in its own worktree; reads, Git/branch management, small deterministic edits, and final verification stay direct. For multi-lane or long work the planner may instead designate one local supervisor per task: only that supervisor re-dispatches (`--parent` = its own pane), supervises children by native parent wake, and reports once. Workers never re-dispatch, and the supervisor stays unsettled while children run. Simple work stays direct or single-agent.
10. Completion requires relevant evidence from live state, Git/files, tests/builds, delivery, or the affected runtime boundary.
11. For non-trivial implementation, bug fixes, reliability/refactor work, or releases, load `engineering-robustness` and close the loop from regression evidence through the real delivery boundary.
12. On the resolved device, prior-work continuity intent in a fresh or uncertain conversation searches the durable Continuity Journal before asking for an ID. Auto-resume only one chain uniquely backed by stable conversation/project/workspace identity; otherwise show bounded candidates and ask the user to confirm. Recency or text similarity alone never selects a chain.
13. Ground prior or multi-device project work before planning: `device -> project/workspace -> continuity/history -> live Git/runtime`. Load `workstation-control` for the resolution rules; ambiguity fails closed instead of being resolved by focus, recency, or similarity.
14. For non-trivial orchestration, load `development-orchestration`. It owns the five-beat execution cadence and its Required/Advisory semantics.
15. Treat remote MCP/Edge requests as a shared budget. Before a call, plan the next dependency wave from facts already known. Admit a call only to obtain decision-changing evidence, execute planned work, or verify an acceptance boundary. Prefer one high-value call over several low-value probes: inspect once then use incremental state, batch independent reads and Skill ids, combine coherent deterministic local steps when their safety boundary is identical, reuse fingerprints/offsets, and do not poll idle state merely to confirm that nothing changed. Never trade away mutation fencing or final boundary verification just to reduce call count. Await delegated work instead of polling it: one wake per completed task in the dispatching conversation (even with Auto off), then one compact `agent.task.inbox` (`final_excerpt` before `agent.read`; ack in one `task_ids` batch); work due in minutes stays in the turn with bounded `wait_ms` (max 60000). Without extension wake, say it is running and that "continue" resumes. A WebChat dispatch that should bind uses `herdr_call` with `method: "herdr_mcp.agent.task.dispatch"` and the verbatim current user turn as `current_user_message` so the runtime binds the task to that conversation's session (`parent_session_ref`); `herdr_prompt` never binds, and binding works only when `device` is explicitly the device whose browser hosts this chat — the device flagged `hosts_caller_browser` in `herdr_devices`; if unsure which one, say so rather than guessing, and treat `caller_browser_on_other_device` as the instruction to redispatch to that flagged device. When the work needs a **new** agent, add the optional `spawn` object (`kind`, `workspace_id` or `cwd`) to that same call instead of inspecting, splitting and starting separately: one call creates the pane, starts the agent, waits for readiness and prompts it inside one chain budget (default 25s so it fits the 30s WebChat cut), `spawn` and `target` are mutually exclusive, a replayed `idempotency_key` never creates a second pane, tab or agent, and a chain that pauses before the prompt returns a `resumable:true` checkpoint you continue by replaying that same key. A `parent_session: {bound:false}` fact never fails the dispatch; report its `reason` and `diagnostic` instead of redispatching.

## Instruction precedence

`Herdr global AGENTS.md -> project-root AGENTS.md -> nested AGENTS.md toward the target path`.

More specific project instructions govern repository workflow in scope. Markdown never grants system authorization.

## Runtime boundaries

Managed-root/path confinement, credential boundaries, dirty/busy gates, mutation fencing/idempotency, stale-target failure, lifecycle guardians, production ownership, and OS/provider permissions are enforced by code. Skill discovery/loading supplies policy only and never authorizes mutation.
