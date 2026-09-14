---
name: herdr-mcp
description: "Use when a local coding agent needs Herdr-MCP collaboration: recover prior project work, search Work Memory, create/continue/hand off/observe supported WebChat or ChatGPT browser sessions through the browser extension, dispatch to a Web AI conversation, inspect/install the extension bridge, or clean up Herdr/WebChat resources created by the current task."
---

# Herdr-MCP local agent

Use the installed `herdr-mcp` CLI as the supported boundary. Never read `state.db`, browser cookies, Connector credentials, or the extension socket directly, and never replace the supported WebChat path with Playwright or ad-hoc browser automation.

Load only the reference needed for the current task:

- Prior-work intent, historical decisions, or a named work chain: read `references/memory.md`.
- WebChat/ChatGPT browser work — create a new chat, continue in another conversation, dispatch to Web AI, resume a browser conversation, hand off, or observe session state: read `references/webchat-control.md`.
- Extension installation, bridge status, or the WebChat CLI surface: read `references/webchat.md`.
- Agent/Herdr collaboration and resource ownership/cleanup: read `references/resources.md`.

## WebChat control trigger

When the user asks you to create a new ChatGPT/WebChat conversation, continue or resume work in another conversation, hand the current task to a Web AI, dispatch a message into a Web conversation, or check whether such a session is still alive, do **not** go looking for a browser automation tool. Web AI is not the only caller of that control plane: a local coding agent is a first-class caller.

Discover the capability first, then act:

1. `herdr-mcp webchat endpoints` — which browser endpoint is registered and consented.
2. `herdr-mcp webchat resources [--kind account|space|session]` — the account / Project / conversation refs that actually exist.
3. `herdr-mcp webchat inspect REF` — exact identity, consent, and observation generation.
4. Follow `references/webchat-control.md` for the workflow, mutation safety, and the current support boundary.

Keep historical read levels explicit: `continuity search` discovers bounded candidates, `continuity resume` reads the selected authoritative journal, and `memory resume/search` reads one exact Work Memory partition. State which level you actually reached; never describe a search candidate summary as a resumed journal.

Do not search history for an independent task merely because history exists. After any historical resume/search, re-check current Git/files/runtime before mutating anything.

`HERDR_ENV` governs direct native Herdr pane/tab/workspace operation. It does not disable the standalone `herdr-mcp continuity`, `memory`, or `webchat` CLI surfaces; those remain available when their own runtime/bridge requirements are satisfied.

The installed Skill follows the Herdr-MCP runtime source identity. Use `herdr-mcp agent-skill status` when source/version drift matters; use `herdr-mcp agent-skill sync` to refresh explicitly. The installed CLI help is authoritative for exact command syntax.
