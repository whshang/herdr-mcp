# Work Memory and continuity

Use history only when the task has real continuity intent: continue/resume/previous work, a named `work_chain_id`, a question about an earlier decision, or a bug/design whose prior evidence materially changes the next action.

## Discover prior work without prompt-injected IDs

When the current task clearly depends on earlier work and no stable chain ID was supplied, search durable Continuity with distinguishing task terms:

```sh
herdr-mcp continuity search "archive retry" [--project-id ID] [--workspace-id ID] [--limit N]
```

Only resume automatically when the result says `auto_resume_safe=true`. If it returns `confirmation_required`, do not choose by recency or textual similarity; surface the bounded candidates to the planner/user. Resume the selected exact chain with:

```sh
herdr-mcp continuity resume CONTINUITY_ID
```

Use stable identity returned by Continuity to enter the exact Work Memory partition when available. Never invent a continuity/work-chain ID.

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
