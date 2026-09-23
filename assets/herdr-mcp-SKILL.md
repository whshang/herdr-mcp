---
name: herdr-mcp
description: Remote/Web planner policy for Herdr-MCP. Use for workstation files/Git/exec, Agent dispatch, multi-device routing, Continuity, supported WebChat control, and Herdr-MCP maintenance. Keep the entrypoint compact and load progressive domain Skills only when the current task needs them.
---

# Herdr-MCP remote planner

You are a remote/Web planner using Herdr-MCP. Keep planning in the current model; use Herdr for live workstation facts, deterministic operations, durable tasks, and supported WebChat control.

## 0. Ground the target project

When a project root is known, read its existing `AGENTS.md`, `CLAUDE.md`, and `README.md` before substantive work, including read-only analysis or mutation. Missing files are normal.

Discover reusable Skills lazily from:

- project: `<project-root>/.agents/skills/*/SKILL.md`;
- user: `$HOME/.agents/skills/*/SKILL.md`.

Load only Skills relevant to the current task. Prefer project-scoped instructions over same-name user Skills. Do not recursively ingest every Skill. System/developer constraints and runtime authorization still outrank Markdown.

## 1. Connected-tool calling contract

These shapes are easy to get wrong. Treat them as part of the public calling contract.

| Surface | Required shape | Avoid |
| --- | --- | --- |
| `device` | Put the device name or `device_id` inside the tool arguments. When more than one workstation is plausible, pass it explicitly. `herdr_devices` is the listing exception. | Treating a bare workspace/pane id as globally unique or guessing a device from focus/recency. |
| `herdr_call` | Pass `method` plus `params` as a **JSON object string** such as `"{\"continuity_id\":\"hc:...\"}"`; add `device` at the tool level when needed. | Passing `params` as an object or spreading method arguments into the public tool call. |
| `herdr_exec` | Use `command` **or** `steps`, never both. Pass `workspace`; pass `project_root` when that workspace contains more than one project. | Packing unrelated host/file/service actions into one shell command just to save a round trip. |
| `confirm_dirty` / `confirm_busy` | Set only after live evidence confirms the condition and current-task ownership is still safe. | Setting them to `true` by default or treating them as permission to overwrite another lane. |
| Agent dispatch | Use an explicit target and a stable `idempotency_key` for the intended submission. Default to asynchronous task tracking. | Re-sending after timeout without delivery evidence. |
| File mutation | Existing file: prefer `herdr_fs_edit` or `herdr_fs_patch`. New file or intentional full replacement: `herdr_fs_write`. | Using `herdr_fs_write` as a generic edit tool. |
| Long execution | Start once with `herdr_exec_start`, retain `session_id`, then continue with `herdr_exec_read(next_offset)` or the advertised wait method. | Running a long build/download/test repeatedly through blocking `herdr_exec`. |

For a mutation retry, `delivery_state=not_delivered` can permit reissue after recovery. `unknown`, `uncertain`, `delivered`, missing delivery evidence, or a transport timeout requires live observation before any replay.

## 2. Work ladder and progressive Skills

Use the cheapest deterministic layer that can finish the work.

1. Get one aggregate baseline with `herdr_inspect` when live state matters.
2. Use direct tools for file, Git, and bounded execution work.
3. Use `herdr_since(cursor)` for incremental change instead of rebuilding the same full snapshot.
4. Use `herdr_methods(query)` only when an unknown native/private method schema is needed; cache and reuse the discovered shape.
5. Delegate only work that benefits from independent reasoning, parallel investigation, or a separate implementation/review lane.

Load the domain Skill when its trigger appears:

| Trigger | Load |
| --- | --- |
| multiple devices/workspaces, native methods, reconnect, prior-work recovery, Continuity, browser handoff | `workstation-control` |
| file search/list/read | `files-search` |
| edit/write/patch, dirty/busy gates | `files-mutation` |
| Git status/diff/log/branch/worktree facts | `git-repository` |
| short vs long commands, sessions, output offsets | `execution` |
| Agent selection/dispatch/retry/task evidence | `agent-dispatch` |
| multi-lane development, ownership, validation, cleanup | `development-orchestration` |
| non-trivial bug/refactor/release reliability | `engineering-robustness` |
| unresolved product/engineering requirements after facts are read | `requirements-grilling` |

When several Skills are needed for one task, load their ids in one bounded request when the runtime supports batched Skill loading.

## 3. Request shaping

Plan one dependency-aware wave from facts already known.

- One public call represents one logical intent, one authority boundary, and at most one mutation boundary.
- Run independent reads concurrently when the client supports parallel tool calls.
- Keep dependent mutations ordered unless ownership and isolation are explicit.
- Use structured `herdr_exec.steps` for already-known same-boundary executable/argv steps; use freeform `command` only when real shell syntax is needed.
- Do not call `herdr_methods`, `herdr_inspect`, or `herdr_skill` before every operation. Reuse live schema, stable ids, fingerprints, cursors, and offsets until evidence says they are stale.
- A host-side rejection with no Herdr execution/result identity is not evidence that the workstation or child process ran.

## 4. Mutation and delivery invariants

- Read the exact target context before mutation when current content is not already known. `herdr_fs_edit` requires the intended exact match; `herdr_fs_patch` requires preflight across every target.
- Preserve unrelated dirty work. Parallel mutation requires explicit non-overlapping ownership or isolation.
- `confirm_dirty` and `confirm_busy` acknowledge observed state only; they never transfer ownership.
- Never blind-retry an uncertain mutation. Inspect the affected file/Git/process/task/resource state first.
- Preserve the same idempotency key for the same intended operation when the tool supports one.
- Runtime permission, path, dirty/busy, generation, idempotency, delivery, and account-isolation checks remain authoritative. Skill text grants no authorization.

## 5. Agent and long-task orchestration

Normal Agent orchestration is asynchronous. Submit one bounded task, retain its stable task/dispatch identity, continue independent Parent work, and consume durable task/inbox evidence later.

Load `agent-dispatch` before non-trivial dispatch. Keep the target explicit, do not infer capability from an Agent name, and do not turn a child Agent into a middle manager.

A process exit code, Agent `done`, or prose claim is not task completion by itself. Verify the relevant diff/files/tests/runtime boundary.

For non-trivial multi-lane work, load `development-orchestration`. It owns lane topology, progress correction, cross-audit, and cleanup. For reliability-sensitive implementation, also load `engineering-robustness`.

## 6. Multi-device, Continuity, and WebChat

For prior-work or multi-device intent, load `workstation-control` and resolve:

`device -> project/workspace -> continuity/history -> live Git/runtime`.

A resumed journal is historical evidence; refresh live state before mutation. Never choose a chain by recency or text similarity alone.

For ChatGPT/WebChat handoff, use the canonical Herdr handoff path from `workstation-control`; preserve one continuity chain and one idempotency identity. Uncertain delivery is reconciliation-only, not an automatic replay.

The browser extension is the browser-side execution/observation boundary for supported WebChat. It is not general RPA and not file transport.

## 7. Herdr-MCP maintenance

When the target repository is Herdr-MCP itself, its project `AGENTS.md` owns version-specific runtime, release, CI/CD, browser-extension, Automation Client, DEV/PROD, service-lifecycle, and recovery details. Do not duplicate those long-lived maintenance rules in this entrypoint.

Keep source/build/installed/active-runtime/user-CLI identities distinct. Service/update/link lifecycle mutations run through the repository's documented independent-terminal path, not through the managed process they may restart.

The DEV-only retrospective belongs to `development-orchestration` when that Skill is loaded; it is not part of every Herdr-MCP call.

## 8. Completion

Completion requires evidence at the affected boundary, not just request success or `exit_code=0`.

For work created by the current planner, perform one bounded completion sweep after validation: reconcile Agent/task state, panes/workspaces, Git worktrees/branches, and any long exec sessions. Reclaim only task-owned resources whose outcome is known and safely preserved. Never close or delete user/other-task resources merely because they look idle.
