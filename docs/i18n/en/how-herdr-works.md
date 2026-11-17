# How Herdr works

*One page: who owns what, how a call travels, and where the durable facts live.*

For the deeper design read [Architecture](architecture.md). This page is the map you check first.

## The moving parts

```text
ChatGPT / Claude / Grok conversation          local coding agent (pi, claude, codex, ...)
        │  MCP + OAuth (Connector)                       │  herdr-mcp CLI (trusted local IPC)
        ▼                                                ▼
                  Cloudflare Edge  ────────  your own Worker
                  OAuth, tool catalog,                   (release-matched to the runtime)
                  delivery evidence, routing
                                │
              authenticated outbound WSS (Link)
                                │
        ┌───────────────────────┼────────────────────────────┐
        │             workstation (no inbound port)           │
        │   herdr-mcp runtime (Rust, generation A/B)          │
        │     ├── governance: roots, delivery, idempotency    │
        │     ├── trusted local IPC socket (extension, CLI)   │
        │     └── loopback HTTP MCP (127.0.0.1:8772)          │
        │   Herdr (TUI): workspaces / tabs / panes / agents   │
        │     └── the PTYs, git checkouts and running tests   │
        └────────────────────────────────────────────────────┘

        Chrome extension (optional return path)
          Native Messaging → mode-0600 socket → same runtime
          Side Panel: live workspace / pane / binding state
          Page-side: bound conversation wake + handoff
```

## Who owns what

| Component | Owns | Does not own |
| --- | --- | --- |
| Web AI (ChatGPT/Claude/Grok) | Goals, planning, verification of results | Local state; it only receives what the runtime returns |
| Cloudflare Edge | Public OAuth/MCP identity, request routing to a `device_id`, per-request delivery evidence | Workstation state; it is a relay, not a second source of truth |
| Link | The workstation's outbound WSS; reconnects and reports delivery | Any state; it is a transport |
| herdr-mcp runtime | Contract, governance (managed roots, idempotency, delivery state), Continuity, Work Memory, agent delegation records | The actual PTY/workspace lifecycle |
| Herdr (TUI) | Workspaces, tabs, panes, agents, PTYs, sessions | Anything remote; it never talks to Edge |
| Browser extension | Browser-side execution, conversation binding, wake, observation | File transport, agent runtime, generic browser automation |
| Local coding agent | Its own task; calls `herdr-mcp` CLI like any local client | A second authority over task state |

## One call, end to end

```text
ChatGPT tool call (herdr_inspect, device=macbook-main)
  → Edge authenticates OAuth, picks the device_id
  → Link already holds an open WSS to that Worker
  → runtime checks the contract + managed roots, calls the Herdr socket
  → Herdr answers from live state
  → runtime returns the result plus delivery evidence
```

Two rules keep this honest:

- **Delivery evidence, not optimism.** Every mutation reports a `delivery_state` (`applied`, `not_delivered`, `uncertain`, ...). A timeout or disconnect means "unknown", never "retry immediately". Replay only `not_delivered`, and reuse the same idempotency key.
- **Refs are observed, never synthesized.** `device_id`, `session_ref`, `endpoint_ref`, `continuity_id`, pane/workspace ids all come from previous calls or the CLI. Nothing is guessed from a label or hostname.

## Device identity and routing

Each enrolled workstation is a fleet **device** with an immutable `dev_*` `device_id` and its own credential. `herdr_devices` (an Edge-local action) lists the fleet; `herdr-mcp device list` shows the same non-secret inventory locally.

```text
first computer   : herdr-mcp worker bootstrap   (deploys the Worker, enrolls device #1)
next computers   : enrolled device → herdr-mcp worker pair
                   new computer   → herdr-mcp worker connect "<pairing-address>"
ChatGPT          : name the device, or let Edge refuse with device_ambiguous
```

A bare `w1:p1` is only unique inside one Herdr server/session — keep the `device_id` (or a `herdr_ref_*` affinity) with pane/workspace ids across machines. See [Multi-device control](existing-worker-connect.md) and [Dual-path control](multi-machine-control.md).

## WebChat lifecycle (local agent → browser)

A local agent drives a supported web conversation through the same runtime — extension, trusted IPC, identity and delivery rules identical to the MCP path:

```text
discover   herdr-mcp webchat endpoints → resources → inspect   (observed refs + observation_generation)
create     herdr-mcp webchat create --source-url URL --message MESSAGE --idempotency-key KEY
send       herdr-mcp webchat send   → dispatch_id + delivery_state
observe    herdr-mcp webchat dispatch-status <dispatch_id>
open/close herdr-mcp webchat open | archive | archive-status
handoff    herdr-mcp webchat handoff --continuity-id hc:... --source-url URL
           (canonical packet; automatic_delivery.completed=true is the only "delivered")
```

## Where the truth lives

| Layer | Contents | Lifetime |
| --- | --- | --- |
| Live state | workspaces, panes, agents, Git, PTYs | Herdr owns it; queried, never cached upward |
| Continuity | durable task journal for one `continuity_id` | survives conversations, browser reloads, runtime restarts |
| Work Memory | bounded partition of history (`project_ref` + `repo_id` + `work_chain_id`) | per work chain |
| Registry/state db | devices, grants, dispatches, generations | local to the runtime |
| The page / conversation | rendering only | replaceable; never a state authority |

## When something looks wrong

Diagnose in the order the request actually travels — never restart a healthy layer to fix a broken one above it:

```bash
herdr-mcp status            # runtime, Herdr, TCC, one-line health
herdr-mcp doctor            # per-layer evidence; default output is concise
herdr-mcp doctor --local    # read-only local snapshot: allow-listed processes,
                            # loopback port 8772, config-file metadata only
herdr-mcp link status       # Link transport and Edge routing state
```

`doctor --local` never reads file contents, environment values or `state.db`; a named instance on a different port shows `listening:false` on 8772 by design. Full failure vocabulary and recovery ladders live in [Troubleshooting](troubleshooting.md).
