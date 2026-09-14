---
name: herdr-mcp
description: Use when a local coding agent needs Herdr-MCP collaboration: recover prior project work, search Work Memory, control supported WebChat sessions through the browser extension, inspect/install the extension bridge, or clean up Herdr/WebChat resources created by the current task.
---

# Herdr-MCP local agent

Use the installed `herdr-mcp` CLI as the supported boundary. Never read `state.db`, browser cookies, Connector credentials, or the extension socket directly, and never replace the supported WebChat path with Playwright or ad-hoc browser automation.

Load only the reference needed for the current task:

- Prior-work intent, historical decisions, or a named work chain: read `references/memory.md`.
- Browser extension, WebChat creation/dispatch/status/archive, or WebChat handoff: read `references/webchat.md`.
- Agent/Herdr collaboration and resource ownership/cleanup: read `references/resources.md`.

Keep historical read levels explicit: `continuity search` discovers bounded candidates, `continuity resume` reads the selected authoritative journal, and `memory resume/search` reads one exact Work Memory partition. State which level you actually reached; never describe a search candidate summary as a resumed journal.

Do not search history for an independent task merely because history exists. After any historical resume/search, re-check current Git/files/runtime before mutating anything.

`HERDR_ENV` governs direct native Herdr pane/tab/workspace operation. It does not disable the standalone `herdr-mcp continuity`, `memory`, or `webchat` CLI surfaces; those remain available when their own runtime/bridge requirements are satisfied.

The installed Skill follows the Herdr-MCP runtime source identity. Use `herdr-mcp agent-skill status` when source/version drift matters; use `herdr-mcp agent-skill sync` to refresh explicitly. The installed CLI help is authoritative for exact command syntax.
