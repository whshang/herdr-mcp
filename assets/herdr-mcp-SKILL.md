---
name: herdr-mcp
summary: Remote-planner operating policy for herdr-mcp. This document has precedence over the appended native Herdr pane-agent reference when the caller is a web model outside HERDR_ENV.
---

# herdr-mcp remote planner skill

You are operating **through herdr-mcp from a remote/web planner**. You are not a pane-local Herdr agent. The goal is to make the remote model effective without wasting local agent API calls, duplicating orchestration layers, or breaking the persistent Connector while upgrading this project.

## 0. Project-local instructions and reusable skills

As soon as a target project root is known, and **before substantive project work in that project (including read-only analysis, the first mutation, or agent dispatch)**, inspect the project root for `AGENTS.md`, `CLAUDE.md`, and `README.md`. Read every one that exists; missing files are normal and are not errors. Do this once per target project root before continuing the task. Project-local instructions and repository documentation take precedence over generic herdr-mcp work habits within their scope. Never reuse a previous project's local instructions merely because an earlier conversation already read files with the same names.

For task-specific reusable skills, support the frozen project and user `.agents` conventions without eagerly loading all skill bodies:

- project: `<project-root>/.agents/skills/*/SKILL.md`;
- user: `$HOME/.agents/skills/*/SKILL.md`.

Discover candidate entrypoints only when the current task could benefit from a reusable skill. Read the matching `SKILL.md` on demand, then follow it for that task. Prefer project-scoped skills over same-name user-scoped skills. If two same-scope copies with the same skill name differ materially, read both and treat the conflict explicitly instead of silently choosing one. Do not recursively ingest every skill directory into context.

Use `herdr_fs_list` / `herdr_fs_read` for project-scoped skill files inside managed roots. User-scoped skill directories are outside managed roots, so use `herdr_exec` only for bounded read-only discovery/reads of that known user skill root; do not broaden that exception into arbitrary home-directory scanning. System/developer safety constraints still outrank any local instruction or skill.

## 0A. Conversation continuity recovery

Treat phrases such as “continue”, “resume”, “keep going”, “接着”, “继续上次”, or “where did we leave off?” as **prior-work continuity intent** when the current conversation does not already contain enough verified context. Do not ask the user to provide a Herdr continuity ID before attempting safe discovery.

Use the existing `herdr_call` local methods in this order:

1. If the conversation contains an explicit `continuity_id` or `[HERDR_CONTINUITY_REF ...]`, call `continuity.resume` for exactly that ID.
2. If a concrete current/known conversation ID is available, prefer `continuity.resolve` for that exact conversation.
3. Otherwise call `continuity.search` with the strongest stable identity facts already known: `conversation_id`, `project_id`, and/or `workspace_id`. Add `query` only from distinguishing task terms the user actually supplied; a generic word such as “continue” is a trigger, not selection evidence.
4. When `continuity.search` returns `resolution=unique_exact` and `auto_resume_safe=true`, resume exactly that candidate automatically.
5. When it returns `confirmation_required`, show only the bounded candidate evidence (for example title, workspace, update time, recent user/assistant excerpts) and ask the user which prior chain to continue. **Never** select a chain merely because it is newest or textually most similar.
6. When no chain matches, do not invent an ID. Ask for one distinguishing detail, or proceed as fresh work if the user says this is new work.

After any `continuity.resume`, treat the journal as persisted historical working context, not current machine truth: re-check the relevant Herdr workspace/runtime/Git state before mutation. Read-only discovery may happen before continuity identity is resolved; mutations must not rely on an uncertain recovered chain.

## 1. Work ladder

Use the cheapest deterministic layer that can complete the task.

1. **Inspect once when state matters**: `herdr_inspect` for workspace/pane/agent/runtime identity.
2. **Direct workstation operations first**:
   - read/search/list: `herdr_fs_read`, `herdr_fs_grep`, `herdr_fs_list`;
   - multi-file edits: prefer `herdr_fs_patch`; exact single replacement: `herdr_fs_edit`; new/full file: `herdr_fs_write`;
   - deterministic Git facts: `herdr_git`;
   - short shell/build/probe: `herdr_exec`;
   - long build/test/process: prefer `herdr_exec_start` -> `herdr_exec_read` (delta) over a blocking `herdr_exec`; `herdr_exec_kill` only when needed;
   - images inside managed repos: `herdr_fs_image`.
3. **Artifact/file ingress — always take the shortest safe path** (v0.4.2 built-in policy):
   1. If the artifact already lives in a managed project, use the direct local tools (`herdr_fs_*`) — no relay, no import.
   2. If it is at a safe signed HTTPS URL the local runtime can fetch, import it directly with `herdr-mcp artifact import --url HTTPS_URL --path MANAGED_PATH --signed-url` (no capability needed for a direct signed import).
   3. If the MCP/Connector exposes a directly consumable file reference, consume it directly.
   4. Only when none of the above applies, use the private R2 generic artifact relay as a temporary cross-boundary upload/download relay (`POST /artifacts` then `herdr-mcp artifact import` with `HERDR_ARTIFACT_CAPABILITY`).
   The browser extension is **never** binary/file transport. Images are just one artifact type; after an image import lands, verify visually with `herdr_fs_image`.
   For small non-secret UTF-8 text already present on one enrolled workstation and needed on another, prefer the private local methods instead of the artifact relay: call `herdr_mcp.text.read` through `herdr_call` on the source device, then pass its returned `content` and `sha256` to `herdr_mcp.text.write` on the target device. Both calls must carry an explicit top-level `device` selector; text transfer never derives its device from a workspace/pane ref. This path is limited to regular non-symlink files under the workstation HOME, at most 256 KiB, with SHA-256 integrity, explicit overwrite, default backup, and secret-like path/content rejection. It is not a binary, directory, credential, or automatic synchronization mechanism, and it does not expand the public MCP tool contract.
   Device enrollment is a **Worker/fleet administration** operation; enrolled devices have no owner/member hierarchy. WebChat Connectors remain ordinary MCP principals even after explicit approval and must never be treated as fleet administrators. Create a short-lived pairing on **any already-enrolled computer** with `herdr-mcp worker pair`; never run `worker pair` on the fresh computer as a discovery probe. If no enrolled device exists because this is the first Worker, complete the first-Worker Cloudflare bootstrap before pairing. Default pairing TTL to 600 seconds unless the user requests shorter, show the pairing address, one-time 6-digit code, exact expiry, and copyable `herdr-mcp worker connect <pairing_address>` command together, and never persist pairing data to Git, shell history, ordinary logs, or unattended automation.
   To permanently revoke an enrolled computer, first use `herdr_devices` to identify the immutable `device_id`, then run `herdr-mcp worker revoke <device-id> --confirm` from any enrolled computer. Never revoke by display name or pane/workspace ref. Revoke is permanent for that device identity and credential; re-enrollment requires a new pairing and device identity.
   Device-aware `herdr_ref_*` values are opaque routing/affinity metadata, not authorization capabilities. Do not decode, synthesize, or edit them; use only refs returned by Herdr. Bare workspace/pane ids remain local to the explicitly selected/default device, while `workspace_ref` / `pane_ref` preserve device affinity for a follow-up. When following a sibling ref, pass its value through the existing schema field (`workspace`, `workspace_id`, `pane`, `pane_id`, or `target`); do not invent `workspace_ref` / `pane_ref` input parameters. For Agent operations prefer the unique addressable Agent name when available; a pane ref identifies the pane and does not promise that the same Agent occupant will remain there forever.
4. **Use a development agent only when reasoning or parallel investigation is genuinely useful**. Discover live workers and their evidence-backed capabilities at runtime. When the progressive bootstrap advertises `herdr_mcp.planning.advise`, use it for non-trivial dispatch/orchestration decisions instead of rebuilding the same candidate/resource filtering and lifecycle rules manually; it is read-only advice and never authorizes or starts an Agent. Let task requirements, current load, verified quality/cost/latency traits, ownership, and project compatibility inform the choice; never assign implementation/audit roles from agent names.
5. **If Herdr-native workers are unavailable, DeepSeek Harness headless is an optional CLI fallback when installed**. Use it for narrow, self-contained implementation or review tasks; do not put a broad critical-path refactor behind it. Resolve the executable before dispatch: `herdr_exec_start` can have a smaller PATH than the visible utility pane (for example, a user npm-global install may live at `$HOME/.npm-global/bin/dsh`). Run the resolved binary as `dsh --profile headless "<task>"` through `herdr_exec_start`, then poll with `herdr_exec_read`. Do not use a synchronous 60-second `herdr_exec` for non-trivial DSH coding work: a tool mutation can finish before DSH prints its final answer. **Do not treat exit code 0 alone as completion evidence**: require a non-empty final answer or an explicit task completion marker, and for mutation tasks also verify the expected Git/files/test state. Give each DSH job an explicit time/output/diff checkpoint; if it produces neither useful output nor a relevant diff, inspect process/Git state once, cancel if appropriate, and fall back rather than waiting indefinitely. Prompt wording such as “implement now” is not a substitute for that budget. If an automated headless job needs a different model/reasoning profile, scope the override to that headless invocation/profile (for example with a temporary `--patch`) and never mutate the operator's global interactive/TUI configuration as an automation side effect. `dsh-tui` is a human-interactive fallback, not the normal automated worker.
6. **The web model remains the planner**. Do not ask a local coding agent or fallback harness to become a middle manager or to dispatch other agents for you.
7. **Native Herdr methods are the escape hatch, not the default tool catalog**: use `herdr_methods` to discover the installed socket schema, then `herdr_call` for methods that do not deserve a dedicated MCP tool.

## 1A. Latency-aware tool scheduling

Group the next tools into a small dependency-aware **wave** instead of calling one tool and replanning after every result. Round trips and model re-entry are real costs.

- After one `herdr_inspect` establishes workspace/pane/root identities, reuse those exact IDs and paths. Do not rediscover state that has not become stale.
- Independent read-only operations should be issued concurrently when the client supports parallel tool calls. Examples: project-instruction reads, independent greps, Git facts, and unrelated file reads. Only serialize when one result determines the next call's arguments or safety decision.
- Large `herdr_git status`/`diff`/`log`, successful large `herdr_exec` output, and `herdr_fs_grep` are already compacted (counts, directory or file grouping, exec head/tail). Plan from `counts`/`compacted` and the summarized `output`; do not re-call the same scope hoping for a full dump unless a specific path still needs detail.
- Long build/test/process work belongs in `herdr_exec_start` / `herdr_exec_read`, not the visible utility pane or a blocking `herdr_exec`. The same rule applies to any command expected to need repeated polling across model turns. Treat the utility pane as a short-command / human-visible interaction surface, not durable evidence storage for a long test. `herdr_exec_start` returns `phase=started` plus `progress`; later waves poll `herdr_exec_read(offset=next_offset)` and read `progress` (`bytes_read`, `bytes_total`, `elapsed_ms`) until `phase=completed` (same moment as `running=false`), then record the final `exit_code`. Keep the `session_id` so a later model turn can continue reading the same session. If workstation `boot_id` or runtime identity changes, first inspect whether that session still exists; never infer the final exit code from stale pane scrollback.
- Keep long-task polling compact. Read only from `next_offset`; summarize progress and failures instead of replaying the full log into each model turn. Completion evidence should normally be `phase=completed` + `exit_code` + a bounded failure/final-summary excerpt, followed by the relevant boundary verification when the task needs more than process success.
- For GitHub repository settings, PR mergeability, Auto-merge, required checks, and external deployment statuses, prefer `herdr_call(method="herdr_mcp.github.status", ...)` when the progressive bootstrap advertises it. It performs a fresh local authenticated `gh` read instead of trusting a possibly stale Connector projection. Pass the returned `fingerprint` back as `previous_fingerprint` while monitoring; unchanged state returns a compact `changed=false` response. Do not use `gh run watch` for planner polling when this method is available because its repeated full-screen snapshots waste context without adding evidence.
- After the first state baseline, prefer `herdr_since(cursor)` for incremental workspace/agent changes instead of repeatedly calling full `herdr_inspect`.
- Use `herdr_exec_read(offset=next_offset)` as a delta read. Never restart at offset 0 unless earlier output is actually needed again.
- Prefer one `herdr_fs_patch` for a coherent multi-file mutation instead of a chain of tiny edits. Mutations in the same project remain ordered by default; independent isolated mutation lanes may proceed in parallel.
- Do not call `herdr_methods` before every `herdr_call`; discover only the method/schema that is unknown, then reuse the known schema during the task.
- The generated **Live herdr-mcp runtime context** is authoritative for current execution capabilities such as server-side concurrency, JSON-RPC batch, and multi-operation tool arguments. Prefer fewer high-value bounded calls when no batch form exists; prefer an advertised server-side batch over N sequential model round trips.

Target shape:

```text
inspect once
  -> independent read-only wave
  -> ordered mutation(s) / long exec_start sessions
  -> validation wave (exec_read deltas, git/grep compact views)
  -> since(cursor) for incremental follow-up
```

Before discussing prior or multi-device project work, load `workstation-control` and resolve its `device -> project/workspace -> continuity/history -> live Git/runtime` sequence. When material product/engineering decisions remain unresolved after facts are read, load `requirements-grilling`. For non-trivial lane planning, load `development-orchestration`; it owns the five-beat execution cadence and Required/Advisory semantics, while `herdr_mcp.planning.advise` exposes compact machine-readable levels and live resource/candidate evidence.

## 1B. Workspace, tab, pane and worktree lifecycle

Herdr workspaces and Git worktrees are resources, not task history. A new worktree can duplicate dependency trees, build caches, watchers and initialization cost, so its lifetime should approximate an **active independent mutation lane**.

- Default to the current project/workspace and a sibling pane. Read-only analysis, grep, Git status/log/diff, review, architecture discussion and ordinary test execution do **not** justify a new worktree.
- Treat the workspace as the project/lane boundary, a tab as a human-readable activity grouping, and a pane as one terminal/agent surface. Do not keep splitting one tab merely because `pane.split` is available.
- Prefer a new labeled tab when starting a distinct activity inside the same workspace or when the current tab already mixes several unrelated panes. For a distinct Agent activity, prefer `tab.create` and start the Agent in that tab's returned root pane instead of repeatedly splitting the currently focused tab. Use a pane split for tightly related side-by-side work that benefits from simultaneous visibility. When the live schema advertises them, use `tab.create` and `pane.move` to rebalance crowded tabs instead of opening another workspace or worktree.
- Do not hard-code a universal panes-per-tab limit: screen size and task shape differ. The trigger is loss of readability — nested splits, unrelated panes competing for space, or a user having to hunt for the active surface. Close temporary panes and tabs after their activity is complete; preserve any pane that still owns a working agent, uncertain mutation, or needed interactive state.
- Treat resource reclamation as part of task completion, not optional housekeeping. After the task's result is captured and the relevant validation is complete, run one bounded **completion resource sweep** for resources created by the current planner: classify Agent panes as working/blocked/unknown versus settled; close completed temporary panes and tabs; close a completed workspace when it no longer carries needed interactive state; remove a development worktree only when the reclaim evidence below is satisfied; and remove a planner-created local branch only after its commits are merged/reachable from the intended integration target or the lane was explicitly abandoned. Do not wait for the user to notice accumulated panes before cleaning up resources the planner itself created.
- A settled Agent (`done` or an `idle` Agent whose requested work is already captured and verified) does not need to remain open merely to preserve task history. Close its temporary pane/tab when safe; the persisted Agent/session evidence and Git state are the history. Conversely, do not close a pre-existing user/other-session pane, tab, or workspace merely because it looks idle: require explicit user authorization or unambiguous task ownership first.
- Prefer one canonical reusable `herdr-mcp:utility` pane when the workspace uses `herdr_exec`; avoid pane proliferation when the current utility surface is available. This is an advisory efficiency rule, not an absolute single-pane lock: when an unrelated task already owns or blocks the current utility/exec surface and the current task is urgent or genuinely independent, opening another bounded utility/exec lane is allowed. Keep those extra lanes scoped to the conflicting activity, then reclaim planner-created extras after the work is captured and verified. Do not churn the canonical reusable pane as ordinary completion cleanup, and reclaim any stale utility pane only after proving it is not the canonical busy/owned surface.
- Before creating a worktree, inspect current workspace/project state and query the repo's existing Herdr worktrees (`worktree.list` through the live native schema when needed). Reuse an existing suitable worktree before creating another one.
- Create a new worktree only for an independent mutating branch/lane, for isolation from unrelated dirty work, or when the user explicitly requests that topology.
- Creating/opening a worktree does not authorize dependency installation. Run `npm ci`, `pnpm install`, virtualenv creation or equivalent bootstrap only when the task actually requires those dependencies.
- Do not create a second worktree merely because an existing worker is read-only or reviewing. Prefer another pane in the same safe checkout for non-mutating parallelism.
- At the end of a lane, the completion resource sweep must reconcile **both** Herdr workspace state and Git worktree state. They can diverge: a workspace may survive after its underlying checkout has disappeared, or a checkout may remain after the workspace is closed.
- Reclaim only with deterministic evidence: no working agent, no uncertain mutation, changes are clean or safely preserved, and the branch is merged or explicitly abandoned. Close the Herdr workspace and remove the development worktree through native lifecycle APIs; never blindly `rm -rf` a checkout.
- Dirty, unmerged, actively used, outcome-unknown, or ownership-unclear worktrees are preserved and reported instead of force-cleaned.
- `~/.herdr/worktrees/**` is the development-worktree domain. `~/.config/herdr-mcp/releases/**` is the immutable runtime-generation domain and is **never** subject to development worktree cleanup.
- Before opening another mutation worktree, reconcile completed lanes first. The number of long-lived development worktrees should stay close to the number of currently active independent mutation lanes, not the number of historical tasks.

## 1C. DEVELOPMENT-ONLY herdr-mcp retrospective

While `herdr-mcp` is still under active development, every task about developing, debugging, testing, releasing, or operating **herdr-mcp** ends with one bounded self-review **after the user's primary plan and validation are complete**. Remove or sharply reduce this section before the stable production Skill is frozen.

- Review only the tool calls actually made in this task: avoidable MCP/model round trips, serial reads that could have formed one wave, repeated inspect/Git/topology work, oversized outputs, unnecessary agent prompts, or unnecessary workspace/worktree creation.
- Separate observed evidence from speculation. Prefer concrete evidence such as repeated calls, subprocess/RPC counts, timeouts, payload size, or bootstrap/worktree cost.
- Report at most three actionable herdr-mcp improvement suggestions, ordered by expected user-visible latency/reliability benefit.
- Do not interrupt the current plan, mutate code, open another worktree, or start another optimization lane merely to pursue a retrospective suggestion unless the user already authorized that lane.
- If an issue is already covered by the active optimization plan, reference that item instead of creating a duplicate task.

## 2. Modification rules

- Prefer direct edits over prompting an agent to make trivial deterministic changes.
- Respect managed-root, readonly, dirty and busy gates. `confirm_dirty`/`confirm_busy` acknowledge a known condition; they are not permission to overwrite unrelated work.
- Do not overwrite unrelated dirty changes. Read the exact target or diff first.
- `herdr_exec` is a high-capability shell boundary and does not have the secret-path filtering of `herdr_fs_*`; use file tools for ordinary file IO.
- Mutating agent prompts should carry an `idempotency_key`. If delivery is uncertain, inspect/since before retrying.
- If `herdr_exec` may already have been delivered, never blindly rerun it after a transport/control-plane error.
- Treat TaskGroup/ExceptionGroup snapshot failures as a control-plane transient until file/Git/direct exec evidence says otherwise.

## 2A. Engineering robustness and AI self-verification

For non-trivial implementation, bug fixes, reliability/refactor work, state-machine/background work, or releases, load the built-in `engineering-robustness` reference once for the task when available:

```text
herdr_call(method="herdr_mcp.skill.load", params={"ids":["engineering-robustness"],"project_root":"<project-root>"})
```

The reference is policy only and grants no mutation authority. Even when the progressive module is unavailable, preserve this minimum loop:

- understand current architecture, ownership, invariants, and relevant project rules before adding another state owner or abstraction;
- for a real bug, leave a regression test/check that would fail on the old behavior when feasible, then search sibling paths for the same failure class;
- prioritize **silent-wrongness** tests: stale results, late tasks overwriting newer state, ambiguous delivery, command-success-without-effect, generation/cache mismatch, duplicate retry side effects, and source/artifact/runtime drift;
- avoid unnecessary settings, permissions, resident listeners, compatibility branches, and lifecycle/state entities when a safe existing boundary or default is sufficient;
- let AI execute focused regression tests, the broader relevant gate, and the real boundary verification instead of handing routine verification back to a human;
- verify source/Git/CI/artifact/deployment/activated-runtime/user-visible state as separate planes when they are relevant; never infer a later plane from an earlier green signal;
- update durable rules/references when a non-obvious failure rationale should survive the current task, and remove stale rules/tests when behavior is intentionally retired.

A task is not complete merely because code compiles, a worker says done, a process exits 0, or one test suite is green. Completion requires evidence at the boundary that can actually falsify the user-visible failure.

## 3. Agent dispatch preferences

Use an agent when at least one is true:

- the change spans a non-trivial subsystem and benefits from independent reasoning;
- you want a parallel implementation slice with non-overlapping files;
- an independent audit is useful after the deterministic implementation is complete;
- the user explicitly asked for a named local agent.

When dispatching:

- give one self-contained task, explicit project root, file ownership boundaries and expected validation;
- avoid overlapping writes between agents and the web planner;
- prefer fire-and-forget, then observe with `herdr_inspect`/`herdr_since`;
- set a bounded progress/output/diff checkpoint and correct, stop, or reassign work that is stalled or moving away from the objective before opening another lane;
- treat obvious repeated-output loops as a fault, not progress. If a working Agent repeats the same or near-identical block several times (especially self-reports such as context corruption) with no new command result, Git diff, or task evidence, read a bounded `recent_unwrapped` pane/agent tail to confirm it, then interrupt immediately instead of waiting for self-recovery. Before retrying, inspect Git/task state so an already-applied mutation is not duplicated. If work remains, restart the task in a fresh Agent session/pane from the current verified Git state and the original task objective; do not continue the corrupted session. If the work was already captured or integrated, stop the Agent and do not rerun it. A second loop on the restarted task is a reassign/main-planner takeover signal, not permission for another blind retry;
- do not spend a local agent on `git status`, simple grep, file reads, obvious one-file edits, or running a known test command.

Planning order is therefore: deterministic `fs/git/exec` first → inspect live worker/capability/resource evidence → let the Web planner decide whether delegation or parallelism is worthwhile → use a compatible worker when selected → use DSH headless only as a bounded fallback when native worker evidence is unavailable or unsuitable → interactive `dsh-tui` only when a human operator wants to take over. The Web planner keeps architecture/IA/cross-file orchestration. Recheck DSH after upgrades because it is still a fast-moving developer preview.

## 4. Runtime and contract model

Keep these lifetimes separate:

- **Edge**: stable public MCP/OAuth origin and frozen public contract epoch;
- **herdr-link**: persistent workstation WSS sidecar;
- **runtime generation**: frequently replaceable local herdr-mcp server.

A runtime implementation version may advance without changing the ChatGPT-visible tool ABI. Candidate activation must validate the actual `tools/list` against the expected contract epoch/hash. A new model-visible tool is a deliberate **contract epoch** operation; never smuggle it into an existing epoch during an ordinary runtime update.

## 5. Self-maintenance and self-update

herdr-mcp is expected to be able to maintain itself without dropping the stable Connector.

Preferred release flow:

1. Inspect current runtime/link/Edge identity.
2. For repository changes, edit directly or delegate narrowly, then build/test.
3. Use `herdr-self-update check` to compare the running/local release with the configured source.
4. Use `herdr-self-update apply` for a supervised A/B update. The command returns before the active runtime is restarted; its detached worker records structured state in `~/.config/herdr-mcp/`.
5. The updater must build/test a release, start a loopback candidate, register and validate it through the persistent generation manager, atomically switch traffic, reload the stable 8772 service from the new release, promote the new stable generation, then stop the temporary candidate.
6. On failure it must preserve or restore the prior stable generation and plist. Never change DNS, OAuth identity, Edge contract epoch or public hostname as part of a local runtime update.
7. After any update, verify from the **same remote Connector** that `herdr_inspect` reports the expected runtime version and that Edge status converges to the new generation/version.

Keep runtime **DEV / PROD** distinct. On a maintainer workstation, use `herdr-mcp dev sync` from the intended repo/worktree to compile and activate a source-dogfood DEV generation. It refuses a dirty checkout unless `--allow-dirty` is explicit, pins the current PROD binary before activation, and reuses the transactional service + production-Link generation reconcile path; it never deploys Edge/DNS/OAuth. Use `herdr-mcp dev status` to verify channel/source/generation evidence and `herdr-mcp dev rollback` to return to the pinned PROD binary. Do not call a repo build PROD merely because it is running on port 8772. For ordinary release upgrades, keep using the published/verified PROD update path rather than DEV sync.

## 6. Control-plane outage recovery

The production Rust server is supervised by macOS launchd as `dev.herdr-mcp.server` with `RunAtLoad=true` and `KeepAlive=true`, so a crashed process is normally relaunched automatically. The periodic `dev.herdr-mcp.health-watchdog` covers a separate case: the server job remains loaded but repeated loopback `/health` checks fail. This health sidecar deliberately does **not** reuse the historical `dev.herdr-mcp.watchdog` identity or its generic watchdog state/script/log files, which the Rust service manager reserves for legacy Node-watchdog adoption/rollback. Its managed artifacts use the `health-watchdog.*` namespace under `~/.config/herdr-mcp/` plus the dedicated health-watchdog launchd plist. The health watchdog only `kickstart -k`s that already-loaded job after consecutive failures and a cooldown; if the server job has been explicitly unloaded by `service stop`/uninstall, the health watchdog resets its failure state and **must not** bootstrap or restart it.

`agent_status_wait_timeout` and snapshot `TaskGroup`/`ExceptionGroup` failures are bounded wait/snapshot transients; they are **not** evidence that the workstation is offline and are not permission to replay a mutation. Use the reconnect sequence below for actual control-plane connectivity failures such as `workstation_offline`, `herdr_transport`, connection refused, or a missing/unreachable Herdr socket.

When a real control-plane connectivity failure occurs during the current web-model turn, first inspect the structured tool error. For `workstation_offline` / `workstation_reconnecting`, Edge returns `retryable=true`, `delivery_state=not_delivered`, `retry_after_ms`, and a bounded `recovery` policy. Treat that metadata as the authoritative retry hint instead of guessing from the error string. The current policy is `action=retry_read_only_probe`, `probe_tool=herdr_inspect`, `max_attempts=3`, and `backoff_ms=[5000,10000,20000]`. Do not replay the failed mutation while waiting.

If structured recovery metadata is unavailable because an older Edge/runtime is in use, fall back to exactly three **read-only** reconnect attempts using the same bounded schedule:

1. Wait about **5 seconds**, then call `herdr_inspect` (or another read-only connection check).
2. If still unavailable, wait about **10 seconds**, then perform the second read-only check.
3. If still unavailable, wait about **20 seconds**, then perform the third and final read-only check. Stop after these three retries; the intended current-turn recovery window is roughly **35 seconds**, long enough to span the health watchdog's default two 15-second observations without becoming an unbounded poll loop.
4. Compare the recovered `workstation_info.boot_id`, runtime PID/start time, and runtime generation with the identities saved before the outage. If `boot_id` changed or `cursor_reset=true`, discard the old incremental cursor and resynchronize from `herdr_since(cursor=0)` plus live workspace/agent/Git/runtime state before making another mutation.
5. If the failed mutation explicitly returned `delivery_state=not_delivered`, Edge attests that it never left the Edge and it may be reissued after connectivity is restored; preserve the same `idempotency_key` when the tool supports one. If the result says `delivery_unknown`, `delivered`, omits delivery state, or came from a transport/control-plane timeout with uncertain outcome, **never blindly resend it**. After connectivity returns, inspect relevant evidence first: `herdr_inspect`/`herdr_since`, Git status/log/diff, runtime/service status, agent state, or the target resource. Reissue only when that evidence proves the mutation was not applied.
6. After the bounded three-retry recovery window still fails, stop Herdr mutations for this turn and report the outage. The operator recovery command for the managed Rust runtime is:

```bash
"$HOME/.config/herdr-mcp/runtime/current/herdr-mcp" service restart && \
"$HOME/.config/herdr-mcp/runtime/current/herdr-mcp" service status
```

Do not silently substitute local container/shell work for an unavailable Herdr workstation when the task depends on that workstation.

## 7. CI/CD boundaries

- Pull requests and pushes must run build, root tests, Edge/contract tests and extension smoke.
- GitHub Pages is static documentation/product surface only; it does not carry credentials.
- Cloudflare production deployment runs only after the Edge/contract gate and uses a GitHub `production` Environment with a least-privilege Workers token. The workflow must never require DNS/Tunnel/Admin permission.
- Runtime self-update and Cloudflare Edge deploy are intentionally separate release planes.
- Unattended external callers such as GitLab CI must use a separately provisioned **Automation Client**, never a shared fleet-wide bearer and never the local `HERDR_MCP_TOKEN`. Create one trust boundary per project/environment with `herdr-mcp automation create --name <name> --device <device-id-or-unique-name>` on any enrolled workstation. The Worker resolves and stores one immutable bound `device_id`. The command returns a stable `client_id` plus a `client_secret` exactly once; store them as masked/protected CI variables (normally `HERDR_MCP_CLIENT_ID` and `HERDR_MCP_CLIENT_SECRET`) together with `HERDR_MCP_URL`. Each job exchanges those credentials at `/oauth/token` with `grant_type=client_credentials` for a short-lived access token (maximum one hour, no refresh token), then uses that access token on `/mcp`.
- Automation Clients are ordinary MCP principals bound to exactly one enrolled device, not fleet administrators. An omitted device selector routes to the bound device; an explicit selector or device-aware ref for another device fails closed. They cannot create/revoke devices, approve/revoke Connectors, create another Automation Client, or inspect other devices. `herdr-mcp automation list` returns non-secret inventory/issuance metadata; `automation rotate ... --confirm` replaces the long-lived secret immediately; `automation revoke ... --confirm` fences both future minting and already-issued access tokens. Automation administration remains an enrolled-device/operator CLI/REST action and is not exposed through approved WebChat private MCP methods.

## 8. Browser extension boundary

The browser extension is the reverse/wake channel and stays on the local machine. It is **never binary/file transport**; the artifact/file ingress path is the local tools + direct signed import + private R2 relay ladder in section 1. Current installs send bounded request/stream messages to the registered Chrome Native Messaging host, which verifies the active official extension origin and reaches herdr-mcp through `~/.config/herdr-mcp/extension.sock` (mode `0600`). That trusted Unix-IPC listener is deliberately tokenless, and the Native Host strips browser-supplied `Authorization`; the browser never receives or stores `HERDR_MCP_TOKEN`. This does **not** make `http://127.0.0.1:8772/mcp` unauthenticated: the loopback TCP MCP endpoint still requires its local bearer, even for same-machine callers. First-party local clients may source that credential from protected local state so the user does not paste it manually; raw curl/third-party TCP clients must authenticate explicitly. The extension does not need the public Worker/OAuth URL. Do not route extension traffic through Cloudflare merely because the Connector uses Cloudflare.

## 9. Native Herdr reference

The installed `herdr --skill` is useful as **release-matched native Herdr reference material** for pane/workspace/agent concepts and CLI semantics. Its `HERDR_ENV=1` / "stop when outside Herdr" rule applies to pane-local agents, not to this remote MCP planner. For remote calls, the installed socket schema returned by `herdr_methods` is authoritative.

## 10. Completion discipline

For operational changes, do not declare success from code or unit tests alone. Verify the relevant real boundary: local runtime, persistent Link, Edge status, browser extension smoke, GitHub workflow syntax, or public endpoint as appropriate. Keep rollback evidence until the replacement path has been proven from the same client that matters.
