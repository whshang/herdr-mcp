# Collaboration and resource ownership

Herdr workspaces, tabs, panes, agents, worktrees, WebChat sessions, and browser tasks are live resources. Treat them as execution surfaces, not durable task history.

- Read project `AGENTS.md`/`CLAUDE.md`/`README.md` before substantive work.
- Prefer current project/workspace state and deterministic file/Git/command work before creating another agent or worktree.
- Do not let two writers edit the same checkout concurrently. Use an isolated worktree only for a genuinely independent mutation lane.
- Address Herdr resources by returned IDs/names. Do not infer IDs from labels or sidebar order.
- A historical checkpoint is not evidence that current Git/runtime/resources are unchanged.

## Completion cleanup

When the current task created a resource and its result is captured and verified:

1. stop/release the task-owned agent or command if it is still running;
2. close task-owned temporary panes/tabs/WebChat sessions that are no longer needed;
3. remove a task-owned worktree only after the checkout is clean/safely preserved and its branch is merged/reachable or explicitly abandoned;
4. re-list/re-inspect the relevant resource class and verify the resource is gone.

Never close or archive pre-existing user resources or another task's resources based only on idle appearance. Preserve dirty, unmerged, working, blocked, unknown, or ownership-unclear resources and report them instead.

For native Herdr pane/workspace/agent syntax, use the installed Herdr Skill/reference and current `herdr --help`; this Herdr-MCP Skill does not duplicate Herdr's native control manual.
