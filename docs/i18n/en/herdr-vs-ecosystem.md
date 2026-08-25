# Why Herdr + herdr-mcp: tmux, cmux, ACP and coding MCP alternatives

This document records a long-term architecture decision: when a Web AI needs real control over a local development machine, why does herdr-mcp keep Herdr as the persistent runtime instead of replacing it with tmux, cmux, ACP, or copying the product shape of coding-tools-mcp, AgenticGPT, or gpt-webcodex?

**Herdr + herdr-mcp currently best fits the model where the Web AI remains the planner and the workstation remains a persistent, observable, human-takeover-friendly development environment.** Other projects are stronger in narrower layers; this is a boundary decision, not a feature-count contest.

## Evaluation criteria

The control plane needs six properties together: direct access to files/Git/shell/runtime facts; workspaces, PTYs, agents and processes that survive for hours; a return path from workstation events to the Web conversation; mutation semantics that distinguish uncertain delivery from safe retry; human visibility and takeover; and agent neutrality so Claude, Codex, Pi, OpenCode, Grok, Droid and future CLIs remain optional workers.

## Herdr, tmux, cmux and ACP live at different layers

| Option | Primary abstraction | Strength | Main gap for herdr-mcp |
| --- | --- | --- | --- |
| tmux | session / window / pane / PTY | mature, lightweight, SSH-friendly | no agent/project semantics or recovery model |
| cmux | AI-oriented desktop terminal/workspace | strong macOS UX and local interaction | remote Web control and cross-platform runtime are not its core |
| ACP | client ↔ coding-agent protocol | structured session, prompt, permission and events | does not own workstation, PTY, Git/process state or browser continuity |
| Herdr | persistent workspace / pane / agent / event runtime | long-lived state, agent status, human takeover, Socket API | needs herdr-mcp for public MCP/OAuth and Web-facing coding tools |

### tmux is an excellent substrate, but too low-level

tmux already solves durable terminals. A long-running Web planner additionally needs project/workspace identity, semantic agent state, incremental events, safe re-observation after human takeover, and browser-to-workspace binding. Those can be rebuilt on tmux, but the result gradually becomes an agent-aware runtime. Herdr already owns that layer.

### cmux is a strong human frontend, not the remote source of truth

cmux can provide a better local desktop experience by combining terminal, workspace, notifications and browser-oriented workflows. herdr-mcp primarily targets the case where the user may be on another device and the development machine must stay reachable, observable and recoverable for hours. Runtime identity and event semantics come before desktop presentation.

### ACP is useful for agent compatibility, not workstation control

ACP standardizes communication between a client and a coding agent. It is a natural future compatibility layer. The herdr-mcp control plane still needs workspace, repository/worktree, PTY, process, Git state, long exec, runtime generation, browser binding and handoff. Making ACP the core would still require a workstation runtime beside it. A cleaner boundary is to let Herdr own the environment and optionally use ACP behind an agent adapter later.

## Why Herdr fits a Web-planner architecture

```text
Web AI
  ├─ understand and discuss
  ├─ read files / inspect Git / run tests directly
  ├─ make deterministic changes
  └─ delegate only when another reasoning worker helps
          ↓
       Herdr worker
```

Small edits, research and architecture discussions stay lightweight. Complex development can compose multiple local workers. Forcing every request through another coding agent would turn the Web model into a UI for a second planner and add latency and context translation.

## herdr-mcp vs coding-tools-mcp

coding-tools-mcp is a strong deterministic coding-tools runtime: file/search/patch, Git and PTY/exec exposed through a stable MCP catalog with workspace confinement, permission modes, bounded results and reproducible benchmarking.

herdr-mcp should adopt patterns such as stable schemas, baseline-aware patching, bounded output, server-enforced security and deterministic dogfood metrics. Its extra responsibility is the persistent Herdr environment, stable public Edge, workstation routing, runtime generations and browser continuity. If file/Git/exec were the whole problem, coding-tools-mcp would be the simpler shape.

## herdr-mcp vs AgenticGPT

AgenticGPT demonstrates a capable remote-worker model with Secure MCP Tunnel, optional Hub, managed jobs, path/command policy, confirmation, downstream MCP and tmux workspaces. It is a useful reference for bounded jobs and recovery.

herdr-mcp treats the Herdr workspace as a long-lived development site, lets the Web planner choose direct deterministic tools or optional agents, decouples the public Connector from local runtime generations, and maintains a browser return channel. Its center of gravity is continuous development rather than putting every operation into a Job system.

## herdr-mcp vs gpt-webcodex

gpt-webcodex packages Coding Tools MCP into a Windows desktop product with worktree isolation, background tasks, heartbeat, recovery, notifications and runtime/schema identity. Its strongest lessons are product experience and lifecycle clarity.

herdr-mcp should absorb those ideas without requiring every interaction to become a managed task. A workstation may contain multiple projects, shells, development servers and arbitrary agents, while a Web conversation may only be doing research or discussion.

## The closed loop is the differentiator

Downstream control:

```text
Web AI → MCP/OAuth → Edge → outbound link → Rust herdr-mcp
       → files / Git / exec / Herdr Socket API
```

Upstream continuity:

```text
Herdr events → herdr-mcp → local IPC / Native Messaging
             → browser extension → Web conversation
```

Many coding MCP servers solve only the first direction. That is enough for short tasks. When work runs for hours while the user leaves the screen, the return path closes the loop.

## Why task management stays intentionally light

DSH, Luvus and similar systems show useful Team, DAG, lease, mailbox and orchestration ideas. herdr-mcp should not turn them into mandatory workflow machinery. Lightweight metadata such as `work_id`, scope, acceptance criteria, operation state and evidence can improve recovery and observability without requiring a formal task before a one-off read or command.

Task semantics may assist complex work, but must not become the entry fee for simple work. This also keeps local agents replaceable workers rather than product dependencies.

## What to absorb, reuse and avoid

Absorb security, bounded results and benchmark methods from coding-tools-mcp; runtime identity and recovery UX from gpt-webcodex; bounded-job and confirmation ideas from AgenticGPT; lightweight scope/evidence ideas from richer orchestrators; and capability-negotiation ideas from ACP for optional compatibility.

Reuse Herdr for workspace/pane/PTY, agent lifecycle and semantic status, event streams, worktrees, advanced native operations, and human attach/focus/inspection.

Keep out of the main line for now: a second agent runtime, another terminal multiplexer, a full Team/Task DAG/Lease implementation, mandatory ACP internally, dependency on one coding-agent brand, and a general browser-automation framework.

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

Responsibilities stay narrow: Web AI is the planner; herdr-mcp is the secure remote-control and continuity layer; Herdr is the persistent source of runtime truth; local agents are replaceable workers; the browser extension is a return channel, not a reasoning system.

## When another option is better

Use tmux when terminal multiplexing is all that is needed. Prefer cmux for a local macOS desktop-terminal experience. Prefer ACP for client-to-coding-agent interoperability. coding-tools-mcp is simpler for standalone safe file/Git/exec MCP. AgenticGPT is compelling for a Linux remote-worker/Hub deployment. gpt-webcodex is closer to an out-of-box Windows ChatGPT coding desktop product.

Herdr + herdr-mcp is strongest when the goal is to **keep a strong Web model as the primary thinker while giving it durable, reliable and observable control over a real development workstation, with the user free to leave, return and take over at any time.**

## Roadmap constraint

Future work should prioritize Rust single-binary and cross-platform productization; mutation identity, delivery phase, idempotency and recovery; a reliable browser-continuity state machine; work context and evidence; and installation/update/rollback/doctor/observability.

Every new capability should still answer one question: does it make Web AI more reliable at controlling the real development environment? If it mainly duplicates Herdr, another agent runtime or an existing product, reuse that layer instead of growing the public surface.
