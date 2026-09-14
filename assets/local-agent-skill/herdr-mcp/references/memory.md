# Work Memory and continuity

Use history only when the task has real continuity intent: continue/resume/previous work, a named `work_chain_id`, a question about an earlier decision, or a bug/design whose prior evidence materially changes the next action.

## Read levels

Treat these as different evidence levels and say which one you reached in your report:

1. `continuity search`: candidate discovery only. It returns bounded identity/title/workspace/excerpt evidence and may expose an exact Work Memory locator, but it does **not** read the full journal.
2. `continuity resume`: authoritative durable journal for one safely selected `continuity_id`.
3. `memory resume/search`: bounded checkpoint/turn/evidence access inside one exact `project_ref + repo_id + work_chain_id` partition.

Never call level 1 “journal recovery”. Never invent ids to reach levels 2 or 3.

## Discover prior work without prompt-injected IDs

When the current task clearly depends on earlier work and no stable chain ID was supplied, search durable Continuity with distinguishing task terms:

```sh
herdr-mcp continuity search "archive retry" [--project-id ID] [--project-path PATH] [--workspace-id ID] [--limit N]
```

When the checkout is known but the internal ChatGPT Project id is not, prefer `--project-path /path/to/repo`. Herdr resolves that checkout's Git `origin` to the same canonical `repo_id` used by Work Memory and uses it only as a scope filter. Older Continuity chains may predate repo binding; a query hit can therefore appear with `repo_scope=legacy_unbound` and `work_memory=null`. Treat that as an unverified legacy candidate, not as proof that the chain belongs to the requested repository. Such a candidate never reports `auto_resume_safe=true` and stays `confirmation_required`. Scoping by repo/path without a query stays exact-only: it returns just the chains that carry a matching Work Memory repo binding. A repo/path match alone is never permission to auto-resume.

Only resume automatically when the result says `auto_resume_safe=true`. If it returns `confirmation_required`, do not choose by recency or textual similarity; surface the bounded candidates to the planner/user. Resume the selected exact chain with:

```sh
herdr-mcp continuity resume CONTINUITY_ID
```

When a candidate contains a complete `work_memory` object, it supplies the exact `project_ref`, `repo_id`, and `work_chain_id`. Use that locator only after the candidate is safely selected (`auto_resume_safe=true` from a stable identity, or explicit planner/user confirmation). If `work_memory` is null, stop at the Continuity evidence level; never synthesize a partition tuple from the repository path, project title, workspace id, or candidate text. Do not query every candidate's Work Memory to bypass a `confirmation_required` result.

## Reporting prior status without selecting a chain

For requests such as “查一下以前做到哪里 / what did we do before?”, use this safe reporting flow:

```text
continuity search
  -> report bounded candidates and the evidence level reached
  -> do not select a confirmation_required candidate
  -> re-check current Git/files/runtime/docs independently
  -> clearly separate historical candidate evidence from current live state
  -> explicitly say that continuity resume was not performed
```

Historical summaries can be stale. A newer Git commit, merged PR, runtime generation, or current status document must be reported as live evidence, not attributed to the Continuity journal.

## Resume a known work partition

```sh
herdr-mcp memory resume <project_ref> <repo_id> <work_chain_id> [--max-turns N]
```

This returns the verified checkpoint plus bounded recent turns/evidence for exactly that partition. Treat it as historical working context, not live machine truth.

## Search a known work partition

```sh
herdr-mcp memory search <project_ref> <repo_id> <work_chain_id> <query> [--limit N]
```

Use distinguishing task/design terms from the actual request. Keep searches narrow; do not scan unrelated projects or chains.

## Working rule

After resume/search, inspect the current repository/runtime state before mutation. Git, files, active Herdr resources, and runtime generation may have changed since the persisted evidence was written.

Do not read Herdr-MCP SQLite files directly. Do not copy an entire WebChat or terminal transcript into an agent prompt when a checkpoint/search hit is enough. Prefer bounded decisions, implementation results, commits, tests, evidence refs, and next actions.

## HERDR_ENV boundary

An empty/unset `HERDR_ENV` means the current process is not a pane-local Herdr agent and must not assume direct pane/tab/workspace control. It does **not** block `herdr-mcp continuity search`, `continuity resume`, `memory ...`, or supported `webchat ...` commands; those use Herdr-MCP's own local runtime boundary.
