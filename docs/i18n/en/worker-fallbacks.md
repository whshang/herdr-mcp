# Agent delegation

*When to delegate work to a local Agent and when to stay direct.*

herdr-mcp treats the Web AI as the high-level planner. Local coding agents are replaceable execution workers, not a second orchestration layer.

This page answers two practical questions:

1. Which tasks are worth delegating to a local worker?
2. If the preferred worker is unavailable, stalled or times out, how do you switch without repeating mutations that may already have happened?

## First decide whether the task needs an agent at all

Many development actions are deterministic:

```text
read a file       → herdr_fs_read
search code       → herdr_fs_grep
inspect Git       → herdr_git
make exact edits  → herdr_fs_edit / patch
run a command     → herdr_exec
run long tests    → herdr_exec_start/read
```

Delegate only when independent reasoning adds real value, for example:

- understanding an unfamiliar subsystem and proposing an implementation;
- implementing a narrow, self-contained feature;
- investigating a separate hypothesis in parallel;
- independently reviewing a completed diff;
- comparing multiple technical approaches.

The rule is simple:

> Do deterministic work directly. Delegate work that benefits from independent reasoning.

## Recommended worker order

| Priority | Worker type | Typical entry | Good fit |
|---|---|---|---|
| 1 | Herdr-native coding worker | `herdr_prompt` | narrow implementation, research, review |
| 2 | another available Herdr-managed worker | `herdr_prompt` | alternative implementation, parallel validation |
| 3 | external headless coding-agent CLI | `herdr_exec_start` | bounded fallback work when native workers are unavailable |
| Human | interactive TUI / shell | manual takeover | approvals, recovery, complex diagnosis |

Brand or model name is not the durable rule. A worker is a good default when it can:

- run headlessly and predictably;
- accept a narrow task boundary;
- expose observable state;
- produce verifiable results;
- allow the planner to determine whether code changed after a timeout.

## Why Herdr-native workers come first

A Herdr-managed worker already lives inside a visible workspace/pane lifecycle, so the Web planner can observe:

- working / idle / done state;
- pane output;
- cwd;
- workspace ownership;
- prompt delivery evidence;
- `idempotency_key` behavior.

Typical flow:

```text
herdr_prompt
  ↓
herdr_since / herdr_inspect
  ↓
Git / tests verification
```

That is easier to orchestrate over long periods than a completely independent CLI process.

## Where external CLI workers fit

Some coding agents expose a headless CLI and can serve as fallback workers.

Use this model:

```text
Web planner
  ↓
herdr_exec_start
  ↓
external coding CLI
  ↓
Git / tests
```

Do not turn the external CLI into another planner and ask it to decide how to delegate further. The Web planner should narrow the job first.

A good task contract states:

- exact repository;
- file or feature boundary;
- no unrelated edits;
- completion criteria;
- verification command;
- no further agent delegation.

## Why external coding agents should use long-running exec sessions

A coding agent may finish the code change well before it prints its final natural-language summary.

With a synchronous command, the failure mode can look like this:

```text
CLI already changed the code
      ↓
wait for final model summary
      ↓
client timeout
      ↓
assume failure
      ↓
submit the same task again  ← dangerous
```

Prefer:

```text
herdr_exec_start
  ↓
herdr_exec_read
  ↓
inspect Git / tests
  ↓
herdr_exec_kill only when cancellation is actually needed
```

A process timeout and a failed coding task are not the same thing.

## After a timeout, inspect facts before retrying

For any coding worker timeout:

```text
1. inspect worker/pane/process state
2. git status
3. git diff
4. inspect target files
5. run relevant tests
6. only then choose continue / fix / cancel / retry
```

If a relevant diff already exists, a mutation has at least partially happened.

The right next action is usually to verify, ask the existing worker to finish, or let the Web planner fix a small remainder — not to resend the whole original task.

## How to recognize a genuinely stalled worker

Do not use “no final answer for a few minutes” as the only signal. Better evidence includes:

- process or pane remains active but output stops changing;
- no relevant Git diff appears;
- process activity no longer advances;
- the worker repeats the same reads without producing new evidence;
- the task has exceeded a reasonable budget for its scope;
- the critical path is blocked with no new information.

Then:

1. read state one more time;
2. if there is no mutation evidence, cancel;
3. continue with deterministic tools or another worker.

Do not keep waiting indefinitely just because time has already been spent.

## When parallel workers help

Good parallelism:

```text
worker A → implementation
worker B → independent review
```

or:

```text
worker A → investigate browser layer
worker B → investigate server layer
```

Bad parallelism:

```text
worker A, B and C all edit the same file
```

unless each works in an isolated worktree and the Web planner explicitly owns the final integration.

## Isolate real parallel development with worktrees

When multiple workers need to edit code:

```text
main worktree
    │
    ├─ worktree A → implementation
    └─ worktree B → alternative / review fix
```

This avoids:

- dirty-file gate conflicts;
- workers overwriting each other;
- uncertainty about which worker produced which diff;
- one worker's reset/format operation affecting another.

The Web planner compares diffs and test evidence before choosing merge, cherry-pick or manual integration.

## Review workers should not become the default primary editor

Independent review is valuable because it is independent.

Recommended sequence:

1. implement through the primary path;
2. run deterministic tests;
3. give the diff to an independent review worker;
4. let the Web planner judge whether findings are valid;
5. fix small issues directly, delegate larger corrections only when useful.

If the reviewer owns the whole implementation from the beginning, much of that independence disappears.

## Revalidate external workers after upgrades

External agent CLIs evolve quickly, so long-lived docs should not freeze version-specific assumptions.

After an upgrade, recheck:

1. `--version`;
2. headless/non-interactive help;
3. a no-tool answer smoke test;
4. a tiny edit in a temporary Git repository;
5. whether Git evidence reveals mutations after timeout;
6. profile/plugin loading if the worker depends on them.

Only then put that worker back on an automated critical path.

## Human takeover is a first-class capability

Some tasks are better handled by a person:

- OAuth or login;
- security approval;
- interactive TUI workflows;
- visually complex state;
- high-risk external mutation;
- a worker whose behavior has become unpredictable.

Herdr's visible workspace/pane model makes manual observation and takeover part of the architecture rather than an emergency escape hatch.

## Recommended delegation flow

```text
Inspect
  ↓
Can deterministic tools do it?
  ├─ yes → fs/git/exec
  └─ no
       ↓
   define one narrow worker task
       ↓
   dispatch
       ↓
   since / process read
       ↓
   Git + tests
       ↓
   independent review when useful
       ↓
   Web planner decides the next move
```

## Final rule

Workers are replaceable execution resources. Project state is the source of truth.

Completion is never merely:

> “The agent says it finished.”

Completion means:

- the diff is correct;
- tests pass;
- runtime state matches expectations;
- important side effects are accounted for.

That lets herdr-mcp work with different agents without binding the orchestration architecture to one model or CLI.
