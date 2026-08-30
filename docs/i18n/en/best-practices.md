# Best practices: let the Web planner decide, let the workstation provide facts

The most reliable herdr-mcp workflow follows one rule:

> The Web model owns goals, sequencing and decisions. Deterministic workstation work is done directly. Local agents are used only when independent reasoning or parallel work adds value.

This avoids both extremes: turning every action into an agent task, or treating a long-running development environment like a stateless shell API.

## 1. Inspect live state before mutation

A handoff packet, prior assistant message or old terminal output is historical context, not proof of current state.

Start by checking:

- current Herdr workspace/panes/agents;
- target Git root;
- Git status/diff;
- relevant runtime/service health when the task depends on it.

Typical entry:

```text
herdr_inspect
  ↓
herdr_git status
  ↓
herdr_fs_read / grep
```

If the conversation was resumed after a long gap, runtime restart or browser handoff, this rule matters even more.

## 2. Do deterministic work directly

Do not ask an agent to perform operations whose result is already mechanically defined.

Prefer:

```text
read/search files   → herdr_fs_*
Git facts           → herdr_git
exact edits/patch   → herdr_fs_edit / patch
short command       → herdr_exec
long tests/build    → herdr_exec_start/read
```

This saves model context, lowers latency and gives the Web planner direct evidence.

## 3. Delegate one bounded reasoning task at a time

Good delegation tasks include:

- investigate one subsystem;
- implement one narrow feature;
- compare two approaches;
- review a completed diff;
- explore an independent hypothesis in parallel.

A good prompt specifies:

- repository and working area;
- exact problem boundary;
- files or interfaces owned by the worker;
- what not to modify;
- expected tests/evidence;
- whether the worker should only analyze or may mutate.

The local worker should not become a hidden “manager of managers.” The Web planner owns integration.

## 4. Use worktrees for real parallel edits

If two workers need to modify code independently, give them isolated worktrees.

```text
main worktree
  ├─ worker A worktree
  └─ worker B worktree
```

This prevents dirty-file/busy gates from becoming noise and makes it obvious which diff belongs to which task.

Parallel reads/reviews can share a root; parallel overlapping mutations generally should not.

## 5. Treat Git as the source of truth

An agent saying “done” is not completion evidence.

Verify:

- `git status`;
- `git diff`;
- target files;
- tests/build;
- runtime behavior if relevant.

If an agent times out after it may have mutated the repo, inspect Git before deciding whether to retry.

## 6. Never blindly retry an uncertain mutation

The dangerous remote failure is:

```text
mutation happened
  ↓
response was lost
```

Examples:

- `herdr_prompt` was delivered but status wait timed out;
- a shell command was sent to a pane but the control plane then failed;
- a Cloudflare mutation returned an ambiguous network error;
- a browser handoff seed may already have been submitted.

Correct response:

1. inspect actual state;
2. reconcile what already happened;
3. retry only if evidence shows the mutation did not occur.

Use `idempotency_key` on agent prompts whenever the same intent may be replayed.

## 7. Use `herdr_since` to resume, not to re-read everything

A Web client only runs when the user sends another message. It cannot continuously poll while the conversation is idle.

`herdr_since(cursor)` gives an incremental digest of what changed since the last observation.

Use it for:

- resuming after a long local task;
- checking whether a delegated worker finished;
- continuing after the browser wakes the conversation;
- avoiding a full snapshot on every turn.

If the server restarted and the cursor is no longer valid, use fresh inspect state and continue from there.

## 8. Use browser continuity for time, not for reasoning

MCP sends work from the browser to the workstation. It does not automatically start a new Web turn when a local worker finishes later.

Bind the conversation to the relevant Herdr workspace when the task is expected to outlive the current turn.

Use the extension for:

- progress/settled wakeups;
- stale-view/send-timeout recovery;
- long-conversation handoff;
- JSON→MCP on sites without a native Connector.

Do not treat the extension as another planner.

### Auto and manual handoff

New automation scopes default off. Enable Auto only after you have confirmed the binding and event stream are correct.

The HUD's three preset manual progression actions are mutually exclusive with Auto. **Manual handoff is intentionally different and lives only in the HUD:** where supported, it can start with Auto on or off, pauses source automatic wakes during the transfer, and makes the new conversation inherit the source Auto state.

A handoff packet is historical context; the target conversation still re-checks Herdr/Git/runtime before mutation.

If a user manually starts a new conversation in the same bound ChatGPT Project and says only “continue” / “resume”, do not ask them to find a `continuity_id`. Resolve/search durable continuity first. Text-only recall never authorizes automatic selection; ambiguity must be confirmed before resume, and live Herdr / Git / runtime state must be re-inspected before mutation.

## 9. Keep the public Edge stable while the local runtime evolves

Do not couple a local implementation update to a new public URL.

Preferred split:

```text
Cloudflare Edge / OAuth / public MCP
        stable

herdr-link
        stable connection

local runtime generation
        A/B upgradeable
```

Use Runtime A/B for implementation changes inside the same public contract epoch.

A tool-catalog/schema change is a separate contract migration and should be intentionally rare.

## 10. Keep permissions narrow and honest

herdr-mcp has multiple boundaries:

- `herdr_fs_*` is constrained by managed root and secret-path gates;
- write roots can be restricted;
- read-only mode can block mutation;
- busy/dirty confirmation prevents accidental concurrent edits;
- `herdr_exec` is a stronger shell boundary and is not a sandbox.

Do not weaken all permissions just to solve one path problem. First determine which gate is actually blocking the operation.

## 11. Separate deployment planes

A documentation change should not rotate Cloudflare credentials. A local runtime bugfix should not change the OAuth issuer. A Worker relay update should not replace the Git checkout.

Keep these planes separate:

- documentation / Pages;
- public Edge;
- local Runtime A/B;
- browser extension;
- contract epoch;
- DNS / Custom Domain cutover.

Independent planes are easier to test and easier to roll back.

## 12. Prefer evidence-based stopping conditions

A task is complete when the required evidence is present, not when the conversation sounds finished.

Examples:

```text
code task
  → expected diff + tests

runtime upgrade
  → active generation + real tool call + rollback target

Edge deployment
  → health + workstation + OAuth/MCP

browser continuity fix
  → real target-site behavior + smoke tests
```

This keeps long-running work from drifting into “looks probably fine.”

## Recommended orchestration loop

```text
Inspect
  ↓
Narrow read/search
  ↓
Check Git
  ↓
Do deterministic work
  ↓
Delegate only where useful
  ↓
Run long work with explicit handles
  ↓
Resume incrementally
  ↓
Verify Git/tests/runtime
  ↓
Review / integrate
  ↓
Commit / deploy
```

That loop is intentionally boring. The value of herdr-mcp is that a powerful Web planner can keep applying it to a persistent local development environment without losing state between turns.

Related reading:

- [Architecture](architecture.md)
- [Browser continuity](browser-continuity.md)
- [Worker fallbacks](worker-fallbacks.md)
- [Troubleshooting](troubleshooting.md)
