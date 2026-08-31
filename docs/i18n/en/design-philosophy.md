# Design Philosophy: Connect the strongest reasoning with the real development machine

> **Role:** [Overview](overview.md) explains what herdr-mcp is and [Architecture](architecture.md) explains how the system works. This chapter answers only **why these design principles were chosen**.

Many AI coding products start by building another Coding Agent. herdr-mcp starts from a different observation: Web models are already strong at reasoning, while local development environments are already strong at execution. The missing piece is a reliable, observable connection between them.

This chapter intentionally does not repeat installation steps, component topology, or the ecosystem comparison. It records the principles that future design decisions should preserve.

## Web AI and local coding agents have different strengths

A local coding agent naturally sees repositories, terminals and running processes. A Web AI session provides strong reasoning, model choice, long context and the ability to continue from different devices.

herdr-mcp combines these strengths:

```text
Web AI
+ local files / Git / Shell
+ Herdr persistent workspaces
+ local agent dispatch
+ browser continuity
= a remote, observable coding workspace
```

It does not recreate a terminal UI or another agent runtime. It gives Web AI the hands and eyes it lacks.

## Principle 1: Keep planning in the Web model

The Web model owns high-level goals, decomposition, trade-offs and verification.

Local agents are better treated as independent workers:

- investigate a focused problem;
- implement a bounded feature;
- perform a second review;
- run a long reasoning task.

A Web planner does not need an agent to execute every `git status` or read every file. Fewer translation layers mean less latency and clearer decisions.

## Principle 2: Deterministic work should be direct

Ask: "Does this action really need another reasoning process?"

| Work | Default tool |
|---|---|
| Read/search files | `herdr_fs_*` |
| Git facts | `herdr_git` |
| Precise changes | `herdr_fs_patch/edit/write` |
| Build/test commands | `herdr_exec*` |
| Observe runtime | `herdr_inspect` / `herdr_since` |
| Independent investigation | `herdr_prompt` |

This keeps the workflow closer to an experienced engineer using tools.

## Principle 3: Runtime truth belongs to Herdr

Conversation history describes intent. Herdr describes what is actually running.

After a long task, restart or handoff, the correct action is to inspect the live workspace, not assume old text still describes the machine.

Workspace, pane, cwd, agent state and events are the source of truth.

## Principle 4: Keep the MCP surface focused

Herdr has many native Socket API methods. Exposing every method as an MCP tool would consume context and make tool selection harder.

herdr-mcp keeps a compact public contract. Common operations have dedicated tools; advanced native capabilities remain reachable through dynamic discovery.

## Principle 5: Long tasks should survive your absence

The value of continuity is not unlimited automation. It is removing the need to watch a screen for hours.

```text
Define goal
  ↓
Web AI works
  ↓
Dispatch long task when useful
  ↓
Leave the computer
  ↓
Agent reports completion
  ↓
Browser continuity resumes the conversation
```

Automation provides continuity while permissions and verification remain explicit.

## Principle 6: Treat uncertain writes carefully

In distributed systems, "no success response" does not mean "nothing happened".

A prompt, deployment or commit may already have happened when a network timeout appears. Therefore mutation handling relies on observation, idempotency and delivery state instead of blind retries.

## Principle 7: Clear boundaries create security

herdr-mcp does not pretend that shell access is a sandbox. Allowing shell execution means allowing commands with the workstation user's permissions.

The system makes boundaries explicit:

- managed Git roots;
- readonly mode;
- write roots;
- OAuth identity;
- workstation identity;
- local trusted IPC.

## Recommended workflow

```text
Observe → Understand → Act → Verify → Delegate when useful → Re-observe
```

A reliable AI development workflow always returns to current facts after changes.

## When this approach is valuable

- You want Web ChatGPT with access to your own repositories.
- Tasks last hours and involve multiple agents or terminals.
- You move between computers frequently.
- You want to observe and redirect agents.
- You need a stable remote entry without exposing your machine.

For small local edits while sitting at a terminal, a local coding agent may already be enough. herdr-mcp becomes more valuable as tasks become longer, distributed and collaborative.
