---
name: herdr-mcp
description: "Use when a local coding agent needs Herdr-MCP collaboration or user-approved generic webpage control through BrowserPage and a bounded Jev fast path: recover prior project work, search Work Memory, create/continue/hand off/observe supported WebChat or ChatGPT browser sessions through the browser extension, dispatch to a Web AI conversation, inspect/install the extension bridge, or clean up Herdr/WebChat resources created by the current task."
---

# Herdr-MCP local agent

Use the installed `herdr-mcp` CLI as the supported boundary. Runtime state, browser credentials, Connector credentials, and extension IPC stay behind that boundary. WebChat work uses the supported Herdr path so identity, idempotency, and delivery evidence remain available; Playwright and ad-hoc browser automation are separate stacks and do not carry those Herdr guarantees.

Load only the reference needed for the current task:

- Prior-work intent, historical decisions, or a named work chain: read `references/memory.md`.
- Generic webpage work — inspect, click, fill, verify, use bounded Jev fast path, or author project-local Browser Adapter: read `references/browser-control.md`.
- WebChat/ChatGPT browser work — create a new chat, continue in another conversation, dispatch to Web AI, resume a browser conversation, hand off, or observe session state: read `references/webchat-control.md`.
- Extension installation, bridge status, or the WebChat CLI surface: read `references/webchat.md`.
- Agent/Herdr collaboration and resource ownership/cleanup: read `references/resources.md`.

## WebChat control trigger

For requests to create, continue, resume, hand off, dispatch to, or inspect a ChatGPT/WebChat conversation, use the Herdr WebChat control path. Local coding agents and Web AI use the same control plane.

Discover the capability first, then act:

1. `herdr-mcp webchat endpoints` — which browser endpoint is registered and consented.
2. `herdr-mcp webchat resources [--kind account|space|session]` — the account / Project / conversation refs that actually exist.
3. `herdr-mcp webchat inspect REF` — exact identity, consent, and observation generation.
4. Follow `references/webchat-control.md` for the workflow, mutation safety, and the current support boundary.

Handing the current task to a Web AI (continuation/handoff) has one canonical path: `herdr-mcp webchat handoff --continuity-id HC --source-url URL`. It reuses the canonical handoff preparation, keeps `automatic_delivery` and the manual Copy Prompt byte-identical, requires the target conversation to resume the same `continuity_id`, and never creates a second continuity chain. If it reports `automatic_delivery.completed=false`, nothing was created — hand the user `manual_delivery.copy_prompt` instead of re-running with a new `--idempotency-key`.

Keep historical read levels explicit: `continuity search` discovers bounded candidates, `continuity resume` reads the selected authoritative journal, and `memory resume/search` reads one exact Work Memory partition. State which level you actually reached; never describe a search candidate summary as a resumed journal.

Do not search history for an independent task merely because history exists. After any historical resume/search, re-check current Git/files/runtime before mutating anything.

`HERDR_ENV` governs direct native Herdr pane/tab/workspace operation. It does not disable the standalone `herdr-mcp continuity`, `memory`, or `webchat` CLI surfaces; those remain available when their own runtime/bridge requirements are satisfied.

The installed Skill follows the Herdr-MCP runtime source identity. Use `herdr-mcp agent-skill status` when source/version drift matters; use `herdr-mcp agent-skill sync` to refresh explicitly. The installed CLI help is authoritative for exact command syntax.
