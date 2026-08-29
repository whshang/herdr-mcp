# Web AI × Local Development Environments: Architecture and Product Landscape

> Research date: 2026-08-29. This document compares product boundaries and system architecture rather than ranking projects by stars or feature count. These projects evolve quickly; verify implementation details against their current releases.

## Executive view

Projects that let ChatGPT or Codex operate a local development environment now fall into several distinct families. They may all expose MCP, tunnels, files, Git, and command execution, but the decisive differences are:

1. **Where the user starts the task**: ChatGPT Web, Codex CLI, a desktop application, or any MCP client.
2. **Who owns planning**: the Web model directly calls deterministic tools, or the Web model delegates to a local coding agent.
3. **What durable state exists locally**: a single MCP request, or persistent sessions, tasks, PTYs, agents, browser conversations, and recovery evidence.

| Family | Representative projects | Primary entry | Planning/execution model | Best fit |
| --- | --- | --- | --- | --- |
| General coding MCP runtime | coding-tools-mcp, MCPX, DevSpace Local Artifacts | Any MCP client / ChatGPT | Client model directly calls local tools | Add safe local coding capabilities to an existing AI client |
| ChatGPT → workstation | AgenticGPT, gpt-webcodex, chatgpt-workspace-mcp, chatgpt-mcp, chatgpt-local-coder | ChatGPT Web | ChatGPT planner → local runtime/worker | Operate a development machine from Web/mobile |
| ChatGPT → coding agent | codex-from-chatgpt, codex-chatgpt-bridge | ChatGPT Web | ChatGPT → Codex → local environment | Make Codex the dedicated coding executor |
| Codex → ChatGPT Web | codex-chatgpt-web | Codex CLI | Native Codex workflow → ChatGPT Web model surface | Keep Codex UX while using ChatGPT Web inference |
| Dual-agent collaboration | codex-with-chatgpt | Codex + ChatGPT | ChatGPT planner/reviewer + Codex executor | Explicit plan–execute–review loops |

Herdr + herdr-mcp overlaps the first two families but concentrates on a less common combination: **persistent workstation control plane + browser continuity + replaceable local agents**.

## Architecture families

### ChatGPT directly drives a coding runtime

```text
ChatGPT / Claude / Grok / Cursor
              │ MCP
              ▼
      Coding Tools Runtime
              │
       files / Git / exec
```

Representative projects: coding-tools-mcp, MCPX, DevSpace Local Artifacts.

The runtime stays model-neutral. The client model decides what to inspect, modify, and execute.

### ChatGPT remotely controls a workstation

```text
ChatGPT Web
    │
Secure MCP Tunnel / HTTPS
    │
local runtime / worker
    │
workspace / process / tools
```

Representative projects: AgenticGPT, gpt-webcodex, chatgpt-workspace-mcp, chatgpt-mcp.

The product surface expands beyond tools into installation, tunnel management, permissions, background work, recovery, and local lifecycle management.

### ChatGPT delegates coding to Codex

```text
ChatGPT Web
    │ MCP
    ▼
Codex bridge
    │
Codex app-server / CLI
    │
local repository
```

Representative projects: codex-from-chatgpt and codex-chatgpt-bridge. This reuses Codex's own agent loop but introduces another planner/executor layer between Web Chat and the repository.

### Codex uses ChatGPT Web as its model surface

```text
Codex CLI
   │ local Responses-compatible bridge
   ▼
browser / ChatGPT Web
   │
ChatGPT model
```

Representative project: codex-chatgpt-web. The Codex CLI remains the user's primary interface while the browser becomes the inference surface.

### Dual-agent collaboration

```text
ChatGPT planner/reviewer
       ▲       │
       │ MCP   │ control message
       │       ▼
     Codex executor
```

Representative project: codex-with-chatgpt. Planning/review and execution are deliberately split across two agents.

## Project profiles

### coding-tools-mcp

A model-neutral coding runtime with a stable file/search/patch, Git, PTY/exec, and runtime tool surface. It emphasizes workspace confinement, permission modes, bounded results, atomic patching, and deterministic benchmarking. It can serve local MCP clients through stdio or remote clients through Streamable HTTP/tunnels.

Its strongest lesson for Herdr is that deterministic coding tools should remain stable, compact, and server-enforced. Browser continuity and long-lived agent workspaces belong above this layer.

### MCPX

A local MCP runtime/gateway built around durable Remote Sessions. It explicitly separates transport `Mcp-Session-Id` from durable `remote_session_id`, then associates workspaces, edits, execution tasks, plans, operations, artifacts, skills, upstream MCP, and observations with the durable session.

Its strongest lesson is identity discipline: transport identity, work identity, edit identity, execution identity, and artifact identity should never be reconstructed from logs or conversational text.

### DevSpace Local Artifacts

A self-hosted local workspace MCP with particular attention to binary artifacts and ChatGPT attachment workflows. It adds guarded native artifact downloads and Base64 fallback while enforcing traversal, overwrite, symlink/junction, size, and integrity checks.

Its strongest lesson is to treat artifacts as first-class objects with identity, hash, size, source, media type, and bounded reads instead of repeatedly serializing large payloads into model context.

### AgenticGPT

A Linux remote agent/worker runtime with Secure MCP Tunnel standalone mode as the recommended deployment and an optional centralized Rust Hub. Its central abstractions are managed Jobs, command/path policy, confirmation, skills, downstream MCP, and optional tmux workspaces.

Its strongest lessons are fault-domain isolation and bounded job semantics. Multi-device systems should let each workstation remain independently useful while central infrastructure handles routing and coordination rather than becoming a mandatory execution dependency.

### gpt-webcodex

A Windows desktop product around ChatGPT and an embedded Coding Tools MCP runtime. It integrates tunnel onboarding, workspace selection, worktree isolation, background operations, heartbeats, recovery, notifications, diagnostics, build validation, and runtime/schema identity.

Its strongest lesson is productization: install, doctor, upgrade, runtime identity, worktree safety, and recovery evidence should be first-class user-facing capabilities rather than operator notes.

### codex-chatgpt-web

A bridge that keeps Codex as the native client and maps its model requests onto ChatGPT Web. It manages browser tab leases, parallel task tabs, turn-scoped tool authority, DOM-to-stream translation, context/compaction, launcher restart, draining, and strict DEV/production browser-profile isolation.

Its strongest lessons for Herdr are Browser Lease, Page Epoch, semantic browser liveness, drain contracts, and installation/profile identity. Herdr does not need to reproduce the full Responses API emulation layer.

### codex-with-chatgpt

A dual-agent system where ChatGPT plans and reviews while Codex executes. The MCP bridge remains read-only for ChatGPT; execution authority stays with Codex. Small control messages carry state while files, diffs, and logs are fetched through MCP. Conversation handoff records goal, progress, state, issues, and next step, and the new conversation re-reads live repository facts.

Its strongest lesson is control-plane/data-plane separation. Handoff should carry canonical checkpoint references and state identifiers, not a copy of the entire working set.

## Additional projects worth tracking

### chatgpt-workspace-mcp

A deliberately constrained ChatGPT Web → local workspace MCP. It supports approved project roots, file operations, allowlisted tasks, local commits, and controlled pushes without exposing arbitrary shell execution. It demonstrates that narrower capability surfaces can produce a much easier security model for common personal coding workflows.

### chatgpt-mcp

A Linux-oriented stateless MCP adapter with opt-in capability families for files, shell, services, browser, screenshots, and desktop input. It favors a thin runtime and explicit local policy, representing the opposite end of the spectrum from MCPX's durable session/orchestration model.

### chatgpt-local-coder

A broad ChatGPT Web → local coding MCP with many file, shell, Git, and background-process tools plus session recovery. It is useful as a feature-coverage reference, but a large public tool catalog and broad default machine access also increase permission and audit complexity.

### codex-from-chatgpt / codex-chatgpt-bridge

Both expose local Codex through ChatGPT. Their value is reuse of Codex sandboxing, approvals, tool loop, and coding context. The tradeoff is that Web Chat cannot freely choose between deterministic direct tools and agent delegation.

### OpenAI tunnel-client

This is infrastructure rather than a coding agent. The customer-run client establishes an outbound connection to the OpenAI control plane, receives commands for a tunnel, forwards them to a local MCP server, and returns the result. Secure MCP Tunnel should therefore be treated as a replaceable transport layer, separate from runtime state and workstation identity.

## Comparison by product behavior

| Project | ChatGPT Web entry | Any MCP client | Direct local tools | Fixed coding agent | Durable work/session | Browser continuity | Multi-machine/Hub | Primary focus |
| --- | :---: | :---: | :---: | :---: | :---: | :---: | :---: | --- |
| coding-tools-mcp | ✓ | ✓ | ✓ | — | PTY sessions | — | — | Safe coding runtime |
| MCPX | ✓ | ✓ | ✓ | — | **Strong** | — | — | Remote session/runtime gateway |
| DevSpace Local Artifacts | ✓ | ✓ | ✓ | — | Basic | — | — | Workspace + artifacts |
| AgenticGPT | ✓ | ✓ | Worker tools | Extensible | **Strong Jobs** | — | **Strong** | Linux remote worker |
| gpt-webcodex | ✓ | ChatGPT-oriented | ✓ | Optional workflow | **Strong** | Connection management | — | Windows integrated product |
| codex-chatgpt-web | Indirect | Codex-oriented | Codex tools | **Codex** | **Strong** | **Core** | — | Codex using ChatGPT Web inference |
| codex-with-chatgpt | ✓ | MCP bridge | ChatGPT read-only | **Codex** | Handoff | Conversation handoff | — | Dual-agent collaboration |
| Herdr + herdr-mcp | ✓ | MCP control plane | ✓ | **Replaceable/optional** | **workspace/PTY/agent** | **Bidirectional** | Planned | Persistent workstation control plane |

## Choosing by scenario

If the goal is simply to let ChatGPT edit and test a repository, a thin runtime such as coding-tools-mcp or chatgpt-workspace-mcp is easier to operate.

If several AI clients should share one local capability layer, coding-tools-mcp and MCPX are natural choices: the former favors a stable compact tool runtime, while the latter favors durable sessions and runtime orchestration.

For long-running remote control of Linux machines, AgenticGPT's standalone tunnel and optional Hub deserve close evaluation.

For a packaged Windows experience, gpt-webcodex demonstrates a stronger desktop onboarding and operations model.

If Codex CLI is already the desired user interface and only the model source should change, codex-chatgpt-web is the closest fit.

If ChatGPT should act as architect/reviewer and Codex should always execute, codex-with-chatgpt and ChatGPT-to-Codex bridges fit that explicit dual-agent workflow.

Herdr + herdr-mcp is most differentiated when the Web model should directly handle small deterministic work, orchestrate multiple replaceable agents for larger work, preserve a visible long-lived workstation state, and resume through browser continuity after the user leaves the machine.

## Architectural implications for Herdr-MCP

Herdr-MCP should not converge into another generic Coding Tools MCP or a fixed ChatGPT-to-Codex bridge. Its cleanest boundary remains:

```text
Web AI / ChatGPT
      │ MCP control
      ▼
herdr-mcp control plane
  ├─ deterministic files / Git / exec
  ├─ identity / health / recovery
  ├─ continuity / evidence
  └─ agent orchestration adapter
      │
      ▼
Herdr workstation runtime
  ├─ workspace / pane / PTY
  ├─ process / agent lifecycle
  └─ event stream
      │
      ▼
browser continuity
```

Secure MCP Tunnel, Cloudflare Edge, or another transport should remain replaceable and should never own canonical workstation state.

## Designs worth adopting

**P0 — Durable identity and recovery.** Separate transport sessions from continuity/work identity. Use server-generated identities for tasks, edits, operations, artifacts, runtime generations, browser page epochs, and handoff checkpoints.

**P0 — Semantic health.** A process, socket, or green `/healthz` is insufficient. READY should require protocol handshake, generation/schema agreement, a real request/response probe, and browser-surface semantic liveness when browser control is required.

**P0 — Browser Lease / Page Epoch.** Every controlled tab/conversation should have an explicit lease. Navigation, reload, discard, extension reload, handoff, or generation changes must revoke the old lease and cancel associated observers, timers, listeners, and pending operations.

**P0 — Control plane / data plane separation.** Wake, handoff, and agent messages should carry state, identity, and evidence references. Files, diffs, logs, and test output should be fetched through the data plane on demand.

**P1 — First-class artifacts.** Build reports, screenshots, test reports, attachments, and large logs should have artifact IDs, hashes, sizes, media types, sources, and bounded reads.

**P1 — Independent workstation fault domains.** Central services should route and coordinate while local execution remains useful during central disruption. Heartbeat, recent activity, and analytics must remain bounded.

**P1 — Account-bound release UAT.** Protocol tests should be supplemented by real authenticated UAT for Store/DEV extensions, tunnels, long conversations, reload, runtime restart, generation upgrades, handoff, active-task drain, and child-process cleanup.

## Directions to avoid

- Do not expand the public tool catalog into dozens of narrow operations merely to match feature counts.
- Do not require a heavy Task/Plan object for every simple read, Git query, or one-line edit.
- Do not make one coding agent a mandatory hop; agents should remain replaceable workers.
- Do not treat browser DOM as canonical state; it is a mutable projection.
- Do not build a second terminal/workspace runtime beside Herdr.
- Do not make Tunnel/Edge transport the source of truth for business sessions or continuity.

## Herdr-MCP's differentiated position

File/Git/exec MCP servers are rapidly commoditizing, and Secure MCP Tunnel is increasingly turning safe ChatGPT-to-local connectivity into infrastructure. Durable differentiation is therefore more likely to come from the combination of:

- Web AI remains the primary planner;
- the workstation retains a persistent, visible, human-takeover-friendly workspace/PTY/agent state;
- small work does not require an agent, while complex work can be delegated to multiple replaceable workers;
- browser ↔ workstation continuity is bidirectional;
- runtime, browser, task, and conversation state use stable identities with verifiable recovery;
- transport, model, and agent brands remain replaceable.

## References

Primary projects reviewed:

- https://github.com/xyTom/coding-tools-mcp
- https://github.com/opentokenz/mcpx
- https://github.com/cooky-dance/devspace-local-artifacts
- https://github.com/slhaf/AgenticGPT
- https://github.com/3169657175/gpt-webcodex
- https://github.com/miuuyy/codex-chatgpt-web
- https://github.com/XiaoDuoYa/codex-with-chatgpt

Additional comparisons:

- https://github.com/dxawdc/chatgpt-workspace-mcp
- https://github.com/alexcodeplace/chatgpt-mcp
- https://github.com/posavr/chatgpt-local-coder
- https://github.com/joseanu/codex-from-chatgpt
- https://github.com/Dalomeve/codex-chatgpt-bridge
- https://github.com/openai/tunnel-client

For ChatGPT plan availability, Developer Mode, and Secure MCP Tunnel behavior, verify against current OpenAI documentation and workspace policy.
