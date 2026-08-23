# Worker fallbacks: Pi, DSH and dsh-tui

herdr-mcp keeps the web model as the planner. Local agent CLIs are execution workers, not a second orchestration layer.

## Recommended order

| Priority | Worker | Invocation | Intended use |
|---|---|---|---|
| 1 | Herdr-native cheap worker, especially Pi | `herdr_prompt` | Normal coding/investigation when local reasoning is useful |
| 2 | DeepSeek Harness headless | `herdr_exec_start` running `dsh --profile headless "..."` | Bounded fallback for narrow/self-contained coding or review when Pi/Herdr-native workers are unavailable or a second implementation is useful; do not put broad critical-path refactors behind it |
| 3 | Cline / OpenCode / Anti | `herdr_prompt` when present in a Herdr pane | Alternative Herdr-native coding workers |
| Human fallback | dsh-tui | interactive terminal | Manual takeover, inspection, resume, approvals; not the default automated worker |
| Audit | Droid / Grok | `herdr_prompt` | Independent review after implementation, not primary editing by default |

Deterministic file, Git and shell work should still use `herdr_fs_*`, `herdr_git`, and `herdr_exec` before any agent.

## DSH smoke evidence — 2026-08-23

Tested locally with:

```text
@deepseek-ai/dsh 0.1.1-rc.2
@deepseek-harness-tui/dsh-tui 0.9.0
Node v24.16.0
```

The installed `dsh-tui` profile composes DeepSeek Harness rc.8 plugin packages. Because DeepSeek Harness is still a developer preview, the launcher/profile/plugin versions may move independently and compatibility should be rechecked after upgrades.

### Headless answer

The non-interactive interface is suitable for automation:

```bash
dsh --profile headless "Reply exactly DSH_OK and do not use tools."
```

It returned:

```text
DSH_OK
```

### Controlled code edit

A temporary Git repository was created outside the herdr-mcp working tree with this deliberate bug:

```js
export function add(a,b){ return a-b; }
```

DSH received one task asking it to change only that file so `add(a,b)` returns `a+b`.

Observed result:

- the edit completed successfully;
- `add(2,3)` evaluated to `5`;
- the file was the only changed file;
- the process did not print its final assistant summary within the remote synchronous 60-second budget.

This matters for orchestration: **DSH can edit code successfully, but it should be treated as a long-running worker.** Do not conclude failure merely because no final answer arrived inside 60 seconds, and do not blindly retry after a timeout because the mutation may already have happened.

## Correct DSH invocation from herdr-mcp

Prefer a background exec session:

```text
resolve dsh first (the background exec PATH may be smaller than the visible utility-pane PATH)
  -> command -v dsh
  -> otherwise check $HOME/.npm-global/bin/dsh or the installation-specific bin path
herdr_exec_start(root=<project>, command='<resolved-dsh> --profile headless "<task>"')
  -> herdr_exec_read(...)
  -> inspect Git/tests before retrying if the worker exceeds the expected budget
  -> herdr_exec_kill(...) only when cancellation is actually required
```

Do not assume that a command visible in the persistent utility pane is also on
the `herdr_exec_start` background PATH. In the 2026-08-23 production smoke,
plain `dsh` exited 127 from a background session while the same installation
was available at `$HOME/.npm-global/bin/dsh`. Resolve the executable before
dispatch rather than treating 127 as an agent/model failure.

Recommended task contract:

- identify the exact repository and file/feature boundary;
- tell DSH not to modify unrelated files;
- require tests or a deterministic verification command;
- do not ask DSH to dispatch other agents;
- after a timeout, inspect `git status` / `git diff` before re-submitting.

Unlike `herdr_prompt`, this path does not yet provide Herdr-native agent lifecycle events or idempotency keys. That is why Pi remains the default worker when available.

### Orchestration evidence from real herdr-mcp work

Two independent headless DSH jobs were used against real herdr-mcp bugs:

- **GitHub Pages deployment:** DSH completed and correctly determined that the
  static-site/workflow files themselves were sufficient and that the workflow
  commit had not reached remote `main`. The web planner then resolved the
  machine-specific SSH host-key issue and completed the deployment.
- **Browser workspace/HUD failure:** DSH remained active for roughly seven
  minutes with no stdout and no target-file diff. It was cancelled at the
  orchestration budget and the web planner fell back to deterministic browser,
  socket and network evidence. That investigation found the actual root cause:
  one long-lived `/push/events` SSE per historical binding exhausted Chromium's
  per-origin HTTP connection pool and starved `/push/state`.

This is the intended fallback model: DSH is a capable coding worker, but it is
not allowed to stall the critical path indefinitely. Give it a task-appropriate
budget; if there is no useful output or diff, inspect process/Git state before
cancelling, then fall back to direct fs/exec/browser evidence or another worker.

### Headless scheduling lesson from the docs redesign — 2026-08-23

The documentation-site redesign provided a stricter orchestration test than the
temporary-repository smoke. Several increasingly constrained headless jobs were
given the same isolated worktree: first the broad redesign, then a fixed file
scope, then an exact navigation map and execution-only instructions. They spent
roughly minutes in analysis/tool reads without producing a tracked diff, while
the deterministic web-planner path implemented and validated the change within
the same worktree.

A per-invocation headless configuration override was also tested to reduce
reasoning and to select a faster execution model. That improved trivial no-tool
smokes but did **not** make the multi-file coding or P0/P1 review tasks reliably
fast enough for the critical path. The reusable rule is therefore about task
shape and evidence, not about one provider or model:

- keep architecture, information architecture and cross-file planning with the
  web planner;
- give DSH a narrow task with explicit owned files, expected validation and a
  time/diff checkpoint;
- if the checkpoint arrives with neither useful output nor a relevant diff,
  inspect Git/process state once, cancel if appropriate, and continue with
  deterministic tools or a Herdr-native worker;
- prompt wording such as “implement now” is not a substitute for orchestration
  budgets and completion evidence;
- model/reasoning overrides for automated headless jobs, when useful, should be
  scoped to the headless invocation/profile (for example through a temporary
  `--patch`), not written into the operator's global interactive/TUI profile as
  an automation side effect.

DSH remains useful as an optional fallback and for narrow independent checks;
it is not the default owner of a broad multi-file implementation.

## dsh-tui

The installed TUI profile successfully composes after bringing the local credential document to the current version-1 schema (`version: 1` plus the `refs:` mapping). `dsh --profile dsh-tui --dump-config` then completes successfully.

Use dsh-tui for:

- a human operator taking over an ongoing task;
- browsing/resuming Harness sessions;
- interactive approvals and questions;
- inspecting model/reasoning/profile configuration.

Do **not** use dsh-tui as the normal automated fallback from ChatGPT. A full-screen or interactive terminal has no clean machine-level “final result” contract, while the headless profile does.

## Upgrade rule

Because DSH is a fast-moving developer preview, never hard-code assumptions from one release. Before promoting it in the worker order after an upgrade, verify:

1. `dsh --version`;
2. `dsh --profile headless --help`;
3. one no-tool answer smoke;
4. one temporary-repo edit smoke;
5. `dsh --profile dsh-tui --dump-config` if the TUI is installed.

The herdr-mcp project skill should advertise DSH as an optional installed fallback only when its binary is actually present; the web planner remains responsible for choosing it.
