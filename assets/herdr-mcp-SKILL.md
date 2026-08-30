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
3. **Use a development agent only when reasoning or parallel investigation is genuinely useful**. Prefer cheap/fast Herdr-native workers (`pi`, `cline`, `opencode`, `anti`) for implementation/investigation. Use `droid`/`grok` as independent auditors, not as the primary author by default.
4. **If Herdr-native workers are unavailable, DeepSeek Harness headless is an optional CLI fallback when installed**. Use it for narrow, self-contained implementation or review tasks; do not put a broad critical-path refactor behind it. Resolve the executable before dispatch: `herdr_exec_start` can have a smaller PATH than the visible utility pane (for example, a user npm-global install may live at `$HOME/.npm-global/bin/dsh`). Run the resolved binary as `dsh --profile headless "<task>"` through `herdr_exec_start`, then poll with `herdr_exec_read`. Do not use a synchronous 60-second `herdr_exec` for non-trivial DSH coding work: a tool mutation can finish before DSH prints its final answer. **Do not treat exit code 0 alone as completion evidence**: require a non-empty final answer or an explicit task completion marker, and for mutation tasks also verify the expected Git/files/test state. Give each DSH job an explicit time/output/diff checkpoint; if it produces neither useful output nor a relevant diff, inspect process/Git state once, cancel if appropriate, and fall back rather than waiting indefinitely. Prompt wording such as “implement now” is not a substitute for that budget. If an automated headless job needs a different model/reasoning profile, scope the override to that headless invocation/profile (for example with a temporary `--patch`) and never mutate the operator's global interactive/TUI configuration as an automation side effect. `dsh-tui` is a human-interactive fallback, not the normal automated worker.
5. **The web model remains the planner**. Do not ask a local Claude/OMP/main agent, Pi, or DSH to become a middle manager or to dispatch other agents for you.
6. **Native Herdr methods are the escape hatch, not the default tool catalog**: use `herdr_methods` to discover the installed socket schema, then `herdr_call` for methods that do not deserve a dedicated MCP tool.

## 1A. Latency-aware tool scheduling

Group the next tools into a small dependency-aware **wave** instead of calling one tool and replanning after every result. Round trips and model re-entry are real costs.

- After one `herdr_inspect` establishes workspace/pane/root identities, reuse those exact IDs and paths. Do not rediscover state that has not become stale.
- Independent read-only operations should be issued concurrently when the client supports parallel tool calls. Examples: project-instruction reads, independent greps, Git facts, and unrelated file reads. Only serialize when one result determines the next call's arguments or safety decision.
- Large `herdr_git status`/`diff`/`log`, successful large `herdr_exec` output, and `herdr_fs_grep` are already compacted (counts, directory or file grouping, exec head/tail). Plan from `counts`/`compacted` and the summarized `output`; do not re-call the same scope hoping for a full dump unless a specific path still needs detail.
- Long build/test/process work belongs in `herdr_exec_start` / `herdr_exec_read`, not a blocking `herdr_exec`. `herdr_exec_start` returns `phase=started` plus `progress`; later waves poll `herdr_exec_read(offset=next_offset)` and read result-only `phase` / `progress` (`bytes_read`, `bytes_total`, `elapsed_ms`) until `phase=completed` (same moment as `running=false`). Sync `herdr_exec` / `herdr_fs_grep` still block until done, but completed results include `phase=completed` plus `started_at` / `progress` timing fields for parity.
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

## 1B. Workspace and worktree lifecycle

Herdr workspaces and Git worktrees are resources, not task history. A new worktree can duplicate dependency trees, build caches, watchers and initialization cost, so its lifetime should approximate an **active independent mutation lane**.

- Default to the current project/workspace and a sibling pane. Read-only analysis, grep, Git status/log/diff, review, architecture discussion and ordinary test execution do **not** justify a new worktree.
- Before creating a worktree, inspect current workspace/project state and query the repo's existing Herdr worktrees (`worktree.list` through the live native schema when needed). Reuse an existing suitable worktree before creating another one.
- Create a new worktree only for an independent mutating branch/lane, for isolation from unrelated dirty work, or when the user explicitly requests that topology.
- Creating/opening a worktree does not authorize dependency installation. Run `npm ci`, `pnpm install`, virtualenv creation or equivalent bootstrap only when the task actually requires those dependencies.
- Do not create a second worktree merely because an existing worker is read-only or reviewing. Prefer another pane in the same safe checkout for non-mutating parallelism.
- At the end of a lane, reconcile **both** Herdr workspace state and Git worktree state. They can diverge: a workspace may survive after its underlying checkout has disappeared, or a checkout may remain after the workspace is closed.
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
- do not spend a local agent on `git status`, simple grep, file reads, obvious one-file edits, or running a known test command.

Worker order is therefore: deterministic `fs/git/exec` first → Herdr-native cheap worker (Pi preferred) → DSH headless for bounded fallback tasks → interactive `dsh-tui` only when a human operator wants to take over. The web planner keeps architecture/IA/cross-file orchestration. Recheck DSH after upgrades because it is still a fast-moving developer preview.

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

For development of an uncommitted working tree, `herdr-self-update apply --source working-tree` may stage the current tree into an isolated release directory. For normal unattended upgrades use the committed remote source (`--source remote --ref main` or a release/tag).

## 6. Control-plane outage recovery

The production Rust server is supervised by macOS launchd as `dev.herdr-mcp.server` with `RunAtLoad=true` and `KeepAlive=true`, so a crashed process is normally relaunched automatically. The periodic `dev.herdr-mcp.health-watchdog` covers a separate case: the server job remains loaded but repeated loopback `/health` checks fail. This health sidecar deliberately does **not** reuse the historical `dev.herdr-mcp.watchdog` identity or its generic watchdog state/script/log files, which the Rust service manager reserves for legacy Node-watchdog adoption/rollback. Its managed artifacts use the `health-watchdog.*` namespace under `~/.config/herdr-mcp/` plus the dedicated health-watchdog launchd plist. The health watchdog only `kickstart -k`s that already-loaded job after consecutive failures and a cooldown; if the server job has been explicitly unloaded by `service stop`/uninstall, the health watchdog resets its failure state and **must not** bootstrap or restart it.

`agent_status_wait_timeout` and snapshot `TaskGroup`/`ExceptionGroup` failures are bounded wait/snapshot transients; they are **not** evidence that the workstation is offline and are not permission to replay a mutation. Use the reconnect sequence below for actual control-plane connectivity failures such as `workstation_offline`, `herdr_transport`, connection refused, or a missing/unreachable Herdr socket.

When a real control-plane connectivity failure occurs during the current web-model turn, assume launchd/watchdog supervision may recover it and perform exactly three **read-only** reconnect attempts. Do not replay the failed mutation while waiting:

1. Wait about **5 seconds**, then call `herdr_inspect` (or another read-only connection check).
2. If still unavailable, wait about **10 seconds**, then perform the second read-only check.
3. If still unavailable, wait about **20 seconds**, then perform the third and final read-only check. Stop after these three retries; the intended current-turn recovery window is roughly **35 seconds**, long enough to span the health watchdog's default two 15-second observations without becoming an unbounded poll loop.
4. Compare the recovered `workstation_info.boot_id`, runtime PID/start time, and runtime generation with the identities saved before the outage. If `boot_id` changed or `cursor_reset=true`, discard the old incremental cursor and resynchronize from `herdr_since(cursor=0)` plus live workspace/agent/Git/runtime state before making another mutation.
5. If a mutating call returned a transport/control-plane timeout or otherwise has uncertain delivery/outcome, **never blindly resend it**. After connectivity returns, inspect relevant evidence first: `herdr_inspect`/`herdr_since`, Git status/log/diff, runtime/service status, agent state, or the target resource. Reissue only when that evidence proves the mutation was not applied; reuse the same `idempotency_key` for `herdr_prompt` when replay is justified.
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

## 8. Artifact transport policy

Treat transport as a capability decision, not an image-specific workflow:

- For a local project image that the Web model needs to inspect, prefer `herdr_fs_image`; large sources are represented by a bounded preview while the original file remains unchanged.
- When a remote source already exposes a transferable short-lived public HTTPS capability, prefer direct Rust import (`herdr-mcp artifact import --signed-url ...`) instead of copying bytes through R2. `herdr_inspect.workstation_info.web_artifacts` may expose such an unexpired URL when the optional Thin Web Bridge observed one.
- Cloudflare R2 is provisioned during normal Edge setup as a private generic artifact relay. Use it opportunistically when there is no direct transferable URL, or when a temporary cross-device/cross-session handoff is useful. Objects are bounded to 8 MiB, capability-scoped, and expire after 15 minutes. Delete them after successful transfer when practical. Never treat R2 as a permanent asset library.
- If `web_artifacts` is absent/empty or the browser extension is unavailable, do not poll, repeatedly retry, or block the coding task waiting for it. Continue with normal MCP/local tools; use R2 only when the task actually needs a relay and an authorized upload path exists.

## 9. Browser extension boundary

The browser extension is the reverse/wake channel and stays on the local machine. Current installs send bounded request/stream messages to the registered Chrome Native Messaging host, which reaches herdr-mcp through `~/.config/herdr-mcp/extension.sock` (mode `0600`). The browser receives and stores no Herdr bearer. Static `HERDR_MCP_TOKEN` remains for other local clients and for the native host's old-runtime HTTP fallback only. The extension does not need the public Worker/OAuth URL. Do not route extension traffic through Cloudflare merely because the Connector uses Cloudflare.

For ChatGPT-generated artifacts, the extension may additionally act as a Thin Web Bridge: it observes supported short-lived signed URLs and forwards URL metadata only. It does not upload/download artifact bytes, it does not poll for missing artifacts, and a failed local delivery is a silent optional-feature downgrade.

## 10. Native Herdr reference

The installed `herdr --skill` is useful as **release-matched native Herdr reference material** for pane/workspace/agent concepts and CLI semantics. Its `HERDR_ENV=1` / "stop when outside Herdr" rule applies to pane-local agents, not to this remote MCP planner. For remote calls, the installed socket schema returned by `herdr_methods` is authoritative.

## 11. Completion discipline

For operational changes, do not declare success from code or unit tests alone. Verify the relevant real boundary: local runtime, persistent Link, Edge status, browser extension smoke, GitHub workflow syntax, or public endpoint as appropriate. Keep rollback evidence until the replacement path has been proven from the same client that matters.
