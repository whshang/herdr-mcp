# Quick start

*Prove the first real task after installation.*

> **Role:** this page starts after herdr-mcp is installed and connected. For the executable install contract, use [Agent install](agent-install.md). For manual installation and operations, use [Installation](install.md).

Do not redeploy Edge, reinstall the runtime, or add the browser extension just to follow this page. The goal is simpler: prove that Web AI actually reaches your workstation and can complete one verifiable task in a real repository.

## 1. Start with a read-only check

Open a new ChatGPT conversation and ask:

```text
Show the current Herdr workspace and whether its managed project has uncommitted changes. Use one read-only baseline and do not modify anything.
```

A healthy path looks like:

```text
herdr_inspect
  ↓
answer from workstation facts
```

This proves more than a green Connector badge: the public MCP path, workstation Link, runtime, and Herdr workplace are actually connected.

If this step reports `workstation_offline`, zero tools, an OAuth loop, or a managed-root denial, stop before testing mutations and use [Troubleshooting](troubleshooting.md) to locate the failing layer.

## 2. Make one deterministic change

Choose a safe, easy-to-verify change such as a documentation correction, a test fixture, or a narrowly scoped configuration edit. Ask the Web planner to:

```text
Reuse the current workspace. Read only the Git/file state needed for this change, make one small edit, and combine the relevant verification and final diff where safe. Do not delegate to a local agent.
```

The desired shape is:

```text
reuse baseline → focused read → patch → verification + diff
```

This proves the Web planner can perform deterministic work directly instead of turning every edit into another Coding Agent task.

## 3. Then try a task that deserves delegation

Delegate only when independent reasoning, parallel investigation, or a longer execution really helps. For example:

```text
Investigate the root cause of this failing test and implement the narrowest fix. Keep unrelated files unchanged, then re-check the Git diff and tests yourself.
```

The worker's final prose is not the source of truth. The Web planner should re-read repository state, tests, and runtime facts before accepting the result. Agent selection, timeout, and alternate-worker rules live in [Agent delegation](worker-fallbacks.md).

## 4. Add the browser extension only for long-running Web work

Standard MCP already covers files, Git, shell, agents, and multiple workspaces. Add the extension when you specifically need:

- local `working / progress / settled` events to return to the correct Web conversation;
- page recovery and long-conversation handoff;
- Chrome Side Panel visibility into workspaces, panes, and agents;
- Queue, which sends an explicit next-turn user instruction after the current reply ends.

Keep Auto off on first use and verify binding and live state manually. See [Browser extension](extension.md) for the product overview, [Browser Continuity](browser-continuity.md) for the return path, and [Browser Control Center](browser-control-center.md) for Side Panel operations.

## 5. What counts as a successful first experience

The minimum acceptance requires all four:

1. Web AI can read the real Herdr workplace and repository;
2. a small deterministic edit is verified and visible in the real Git diff;
3. when delegation is useful, the Web planner independently verifies the worker's result;
4. if the extension is enabled, a long task can safely return to the correct conversation without replaying the original mutation.

At that point installation is over. Continue with [Best practices](best-practices.md) for daily workflow and [Architecture](architecture.md) for the system model.
