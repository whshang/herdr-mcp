# Ecosystem comparison

*Why Herdr + herdr-mcp, how the architecture differs, and when alternatives fit better.*

A Web AI that wants real control over a local development machine has many shapes to choose from. This article explains where Herdr + herdr-mcp sits in that ecosystem, why this architecture exists, and when another tool is genuinely a better fit. It is a boundary decision, not a feature-count contest.

The short answer: **Herdr + herdr-mcp is most useful when the Web AI stays the planner and the workstation stays a persistent, observable, human-takeover-friendly development environment.** The Web model performs deterministic small work directly, delegates larger work to replaceable local agents, and keeps the worksite alive across conversations so the user can leave, return, and take over at any time.

## What is being compared

Projects that let ChatGPT or Codex operate a local development environment differ on three decisive axes:

1. **Where the task starts** — ChatGPT Web, Codex CLI, a desktop app, or any MCP client.
2. **Who owns planning** — the Web model calls deterministic tools directly, or it delegates to a local coding agent.
3. **What durable state exists locally** — a single MCP request, or persistent sessions, tasks, PTYs, agents, browser conversations, and recovery evidence.

Because these combinations matter more than any one tool, most projects fall into a few architecture families rather than a long tail of unrelated features.

| Family | Representative projects | Primary entry | Planning/execution | Best fit |
| --- | --- | --- | --- | --- |
| General coding MCP runtime | coding-tools-mcp, MCPX, DevSpace Local Artifacts | Any MCP client / model | Client model calls deterministic tools | Add safe local coding tools to an existing AI client |
| ChatGPT → workstation | AgenticGPT, gpt-webcodex, chatgpt-workspace-mcp, chatgpt-local-coder | ChatGPT Web | Web planner → local runtime/worker | Operate one dev machine from Web/mobile |
| ChatGPT → coding agent | codex-from-chatgpt, codex-chatgpt-bridge | ChatGPT Web | Web → Codex → local repository | Make a dedicated CLI the coding executor |
| Codex → ChatGPT Web | codex-chatgpt-web | Codex CLI | Codex-driven loop using Web inference | Keep the Codex harness while using Web models |
| Dual-agent collaboration | codex-with-chatgpt | ChatGPT + Codex | Web planner/reviewer ↔ Codex executor | Explicit plan–execute–review workflow |
| Persistent worksite control plane | Herdr + herdr-mcp | ChatGPT / any MCP client | Web planner → deterministic tools or replaceable agents | Long-lived workstation + browser continuity |

## The architecture families

### A coding MCP runtime: the model drives tools directly

```text
ChatGPT / Claude / Grok / Cursor
              │ MCP
              ▼
      coding-tools-mcp / MCPX
              │
       files / Git / exec
```

The runtime is model-neutral. The client model decides what to inspect, modify, and execute. coding-tools-mcp emphasizes a stable, server-enforced file/search/patch, Git, and PTY/exec surface with workspace confinement and bounded results; MCPX shows how durable remote sessions give that surface recovery semantics. This is the simplest shape when the whole problem is safe local file/Git/exec access — and it is exactly why Herdr-MCP does not re-implement those tools.

### A remote workstation product

```text
ChatGPT Web → Secure MCP Tunnel / HTTPS → local runtime → workspace / process / tools
```

The product surface expands from tools to installation, tunnel management, permissions, background work, recovery, and local lifecycle management. AgenticGPT (a Linux remote worker with managed jobs and an optional Hub) and gpt-webcodex (a packaged Windows product) live here. Both are strong references for fault-domain isolation and productization, but they tend to put every operation through a job/task system.

### A Codex-first bridge

```text
ChatGPT Web → MCP → Codex bridge → Codex CLI → repository
```

codex-from-chatgpt and codex-chatgpt-bridge reuse Codex's own sandboxing and agent loop, at the cost of making Codex a mandatory execution hop. In the reverse direction, codex-chatgpt-web keeps Codex as the user interface while swapping the model behind it for ChatGPT Web, which is valuable if Codex is already your entry point.

### Dual-agent collaboration

```text
ChatGPT planner/reviewer ↔ Codex executor
```

codex-with-chatgpt splits planning and execution deliberately, with a read-only MCP bridge for the planner and a small control channel carrying state. It is a good model when an explicit two-agent loop is the goal.

## Herdr vs. tmux, cmux, and ACP

These three are often proposed as simpler replacements for the Herdr layer. They solve different problems.

| Option | Primary abstraction | Strength | Main gap for herdr-mcp |
| --- | --- | --- | --- |
| tmux | session / window / pane / PTY | mature, lightweight, SSH-friendly | no agent/project semantics or recovery model |
| cmux | AI-oriented desktop terminal/workspace | strong macOS UX and local interaction | remote Web control and cross-platform runtime are not its core |
| ACP | client ↔ coding-agent protocol | structured session, prompt, permission, events | does not own workstation, PTY, Git/process state, or browser continuity |
| Herdr | persistent workspace / pane / agent / event runtime | long-lived state, agent status, human takeover, Socket API | needs herdr-mcp for public MCP/OAuth and Web-facing tools |

- **tmux** is an excellent substrate but too low-level: a long-running Web planner also needs project/workspace identity, semantic agent state, incremental events, safe re-observation after human takeover, and browser binding. Rebuilding those on tmux gradually becomes an agent-aware runtime — which Herdr already owns.
- **cmux** is a strong local macOS frontend, but herdr-mcp targets the case where the user may be on another device and the development machine must stay reachable, observable, and recoverable for hours. Runtime identity and event semantics come before desktop presentation.
- **ACP** is a natural future compatibility layer for client↔agent communication, but the control plane still needs workspace, repository/worktree, PTY, process, Git state, long exec, runtime generations, and handoff. A cleaner boundary lets Herdr own the environment and use ACP behind an optional agent adapter.

## Why the Web-planner model keeps work lightweight

```text
Web AI
  ├─ read files / inspect Git / run tests directly
  ├─ make deterministic changes
  └─ delegate only when another reasoning worker helps
          ↓
       Herdr worker
```

Small edits, research, and architecture discussions stay lightweight; complex development can compose multiple local workers. Forcing every request through another coding agent would turn the Web model into a UI for a second planner and add latency and context translation.

## The closed loop is the differentiator

Most coding MCP servers solve only the downstream direction:

```text
Web AI → MCP/OAuth → Edge → outbound link → herdr-mcp → files / Git / exec / Herdr Socket API
```

That is enough for short tasks. When work runs for hours while the user leaves the screen, you need the return path:

```text
Herdr events → herdr-mcp → local IPC / Native Messaging → browser extension → Web conversation
```

The browser extension is optional for first setup, but it is the missing second channel for unattended long tasks, page recovery, and cross-conversation handoff. Without it, standard MCP cannot cause an already-settled Web conversation to start a new turn when a local agent finishes.

## What to absorb, reuse, and avoid

Across the ecosystem the durable transferable lessons are:

- **Identity and recovery first.** Separate transport sessions from continuity/work identity; use server-generated ids for tasks, edits, operations, artifacts, runtime generations, browser page epochs, and handoff checkpoints. Never reconstruct identity from logs.
- **Semantic health, not green `/healthz`.** READY should require a protocol handshake, generation/schema agreement, a real request/response probe, and browser-surface liveness when browser control is required.
- **Browser Lease / Page Epoch.** Every controlled tab/conversation should have an explicit lease that navigation, reload, discard, extension reload, handoff, or generation change revokes, cancelling observers, timers, and pending work.
- **Control-plane / data-plane separation.** Handoff and wake messages carry state, identity, and evidence references; files, diffs, logs, and test output are fetched on demand.
- **First-class artifacts.** Build reports, screenshots, test reports, attachments, and large logs carry ids, hashes, sizes, media types, sources, and bounded reads instead of being serialized into model context.
- **Independent fault domains.** Central services route and coordinate; each workstation stays useful during central disruption. Heartbeat, recent activity, and analytics remain bounded.

Herdr-MCP reuses Herdr for workspace/pane/PTY, agent lifecycle and status, event streams, worktrees, advanced native operations, and human attach/focus/inspection. It keeps out of the main line: a second agent runtime, another terminal multiplexer, a full Team/Task DAG/Lease system, mandatory ACP internally, dependence on one coding-agent brand, and a general browser-automation framework. Task semantics (lightweight `work_id`, scope, acceptance criteria, evidence) can assist complex work but must not become the entry fee for a one-off read or command.

## Recommended architecture

```text
                    Web AI
                      │
                MCP + OAuth
                      │
                Stable Edge
                      │
               outbound WSS
                      │
              Rust herdr-mcp
             /        │        \
            /         │         \
       files/Git     exec       Herdr
                                │
                         workspace / pane
                         agent / event / PTY
                                │
                       Native Messaging
                                │
                        Browser continuity
```

Responsibilities stay narrow: Web AI is the planner; herdr-mcp is the secure remote-control and continuity layer; Herdr is the persistent source of runtime truth; local agents are replaceable workers; the browser extension is a return channel, not a reasoning system. Transport (Secure MCP Tunnel, Cloudflare Edge, or another) stays replaceable and never owns canonical workstation state.

## When another option is better

- Need only local terminal multiplexing → use tmux.
- Want a polished macOS desktop-terminal experience → prefer cmux.
- Need client↔coding-agent interoperability → prefer ACP.
- Want standalone safe file/Git/exec MCP → coding-tools-mcp is simpler.
- Want a Linux remote-worker/Hub deployment → evaluate AgenticGPT.
- Want a packaged Windows ChatGPT coding desktop product → gpt-webcodex.
- Prefer Codex as the interface while using Web-model inference → look at codex-chatgpt-web.

Herdr + herdr-mcp is strongest when the goal is to **keep a strong Web model as the primary thinker while giving it durable, reliable, and observable control over a real development workstation, with the user free to leave, return, and take over at any time.**

## References

Primary projects reviewed for this comparison:

- https://github.com/xyTom/coding-tools-mcp
- https://github.com/opentokenz/mcpx
- https://github.com/cooky-dance/devspace-local-artifacts
- https://github.com/slhaf/AgenticGPT
- https://github.com/3169657175/gpt-webcodex
- https://github.com/miuuyy/codex-chatgpt-web
- https://github.com/XiaoDuoYa/codex-with-chatgpt
- https://github.com/dxawdc/chatgpt-workspace-mcp
- https://github.com/alexcodeplace/chatgpt-mcp
- https://github.com/posavr/chatgpt-local-coder
- https://github.com/joseanu/codex-from-chatgpt
- https://github.com/Dalomeve/codex-chatgpt-bridge
- https://github.com/openai/tunnel-client

These projects evolve quickly; verify implementation details against their current releases. For ChatGPT plan availability, Developer Mode, and Secure MCP Tunnel behavior, check current OpenAI documentation and workspace policy.
