# Architecture: give a Web model a workstation that keeps working

herdr-mcp is a remote control plane between a Web AI and a local development environment managed by Herdr.

The key idea is not “put a shell on the Internet.” It is to keep responsibilities separated:

- the Web model owns goals, planning and cross-step decisions;
- Herdr owns persistent workspaces, panes and agent lifecycle;
- herdr-mcp exposes a compact, safe remote-control surface;
- Cloudflare Edge provides a stable public OAuth/MCP identity;
- the browser extension provides the return path into Web conversations plus a Side Panel for observing live local workspace / pane state.

```text
User
  ↓
ChatGPT / Web AI        ← browser continuity ← local Herdr events
  ↓ MCP + OAuth
Cloudflare Edge
  ↓ authenticated routing
persistent herdr-link
  ↓
active local runtime generation
  ↓
Herdr socket + managed Git workstations
  ├─ files
  ├─ Git
  ├─ shell
  └─ agents
```

## The Web model is the planner

The strongest model in the system usually has the broadest context: user intent, prior conversation, architecture choices, task priorities and acceptance criteria.

It should therefore decide:

- what to inspect next;
- what is deterministic enough to do directly;
- when independent local reasoning is worth delegating;
- whether a result is complete;
- what to do after tests, failures or review findings.

Local agents are workers. They should not become a second hidden orchestration hierarchy unless a task specifically benefits from that.

## Herdr is the persistent workshop

A normal Web tool call is transient. A real development job is not.

Herdr keeps the durable work area:

```text
workspace
  ├─ coding pane
  ├─ test pane
  ├─ development server
  └─ review worker
```

That persistent state matters when:

- a browser turn ends while an agent is still working;
- a command runs longer than one MCP request;
- the browser reloads;
- the conversation rolls over;
- the remote planner reconnects after a runtime restart.

herdr-mcp does not replace that model. It exposes it remotely.

## Why the public MCP surface stays small

Herdr has a much larger native Socket API than a Web planner should carry in every MCP tool catalog.

herdr-mcp therefore separates high-frequency capabilities from the long tail.

### High-frequency remote tools

The fixed public surface covers:

- current state: `herdr_inspect`, `herdr_since`;
- project policy: `herdr_skill`;
- files: `herdr_fs_*`;
- Git: `herdr_git`;
- shell: `herdr_exec*`;
- delegation: `herdr_prompt`.

### Native Herdr long tail

Use:

```text
herdr_methods
  ↓ discover live socket schema
herdr_call
  ↓ validated passthrough
native Herdr method
```

This preserves native reachability without turning every Herdr method into a permanent public MCP ABI.

The current production contract is **epoch 2 / 18 tools**. Tool-catalog changes are explicit contract migrations, not incidental runtime changes.

## Progressive skills and capability truth

The epoch-2 tool catalog stays fixed, but the planner policy no longer has to be one giant always-loaded document. The Rust runtime contains a compact global `AGENTS.md` plus eight on-demand modules: workstation control, file search, file mutation, Git, execution, agent dispatch, development orchestration, and engineering robustness/self-verification. Internal `herdr_mcp.skill.list/describe/load` methods are reached through the existing `herdr_call`; they do not add a nineteenth public MCP tool.

The progressive path is deliberately separated from capability truth. A worker is not treated as code-edit capable, vision capable, high-reasoning, or tied to a provider/model merely because of its product name. `herdr-mcp scan` builds evidence instead:

```text
Herdr agent manifest
  + executable/version evidence
  + bounded agent-specific probe
  + live Herdr session state
        ↓
capability inventory
        ↓
capability resolver
        ↓
compact inspect / progressive summary
        ↓
safe dispatch decision
```

Static or semi-static evidence is stored separately from the reliability state database so a new capability schema cannot make an older runtime unable to roll back. Binary identity, manifest version and probe-adapter version invalidate cached evidence. The inventory never owns live status, cwd, project, pane, workspace or session facts; those continue to come from Herdr/EventCache.

Unknown means **unverified**, not false and not “probably supported.” Probe subprocesses are non-interactive, bounded, receive no inherited credentials, and only promote traits explicitly reported by a trusted self-description adapter. Full probe evidence is diagnostic data; the model-visible progressive bootstrap receives only compact counts and verified worker traits.

The Modular Progressive Skills implementation ships in the Rust runtime behind `HERDR_MCP_PROGRESSIVE_SKILLS`. The compatibility/default path remains legacy until capability-aware multi-agent UAT provides evidence for a default-on migration.

## Why files, Git and shell are first-class

A Web model cannot see the workstation filesystem by itself. That is different from Herdr-native pane management.

So herdr-mcp directly exposes deterministic workstation facts and actions:

```text
read/search image → herdr_fs_*
Git facts         → herdr_git
short command     → herdr_exec
long command      → herdr_exec_start/read/kill
```

This avoids wasting an agent call on tasks such as “show me the diff” or “run the test suite.”

## Two communication directions

MCP solves the downward control path:

```text
Web AI → workstation
```

Long-lived development also needs the reverse direction:

```text
workstation → browser conversation
```

The browser extension binds a conversation to a Herdr workspace and can route progress/settled signals, recovery state and handoff control back into the page. Its Chrome Side Panel presents live workspace / pane / agent state, explicit pane targets, bounded reads, and a trusted local control plane: Agent Prompt executes with Rust target fencing and durable idempotency; provider Steer reports exact capability outcomes; terminal-only panes expose a narrow fenced `pane.send_input + Enter` command path; arbitrary Herdr methods remain fail-closed/preview-only.

That extension is not another runtime. Continuity, Control Center, Queue, and JSON → MCP are browser surfaces over the same trusted local bridge, while Herdr remains the runtime truth.

See [Browser continuity](browser-continuity.md).

## Why the workstation connects outward

The local runtime binds to loopback. The public Internet does not connect directly to the workstation.

Instead:

```text
workstation
   └─ authenticated outbound WSS → Cloudflare Edge
```

This creates a stable public endpoint without opening an inbound workstation port.

The public plane can remain stable while the local runtime restarts or changes A/B generation.

## Edge and runtime are separate release planes

```text
Public plane
  Worker / Durable Object / OAuth / MCP endpoint

Local plane
  herdr-link / active runtime generation
```

A local implementation fix should normally not require a new Connector URL. Likewise an OAuth relay fix should not require replacing the local runtime.

See [Cloudflare Edge deployment](cloudflare-edge-deployment.md) and [Runtime A/B](runtime-self-upgrade.md).

## Runtime A/B

`herdr-link` routes new requests to an active local generation pointer.

```text
          ┌─ runtime A :8772
herdr-link
          └─ runtime B :8773
```

A candidate can start independently, pass health and contract gates, become active, and still leave the old generation available for rollback.

Already-dispatched work must not be duplicated merely because the active pointer changed.

## Managed Git roots are the file boundary

Remote file operations are constrained to Git-backed project roots known to the live Herdr snapshot.

Important gates include:

- managed-root validation;
- read-only mode;
- optional write-root allowlist;
- dirty-file acknowledgement;
- busy-project acknowledgement;
- secret-ish path filtering for `herdr_fs_*`.

`herdr_exec` is deliberately a stronger boundary: it runs a shell as the workstation user and is not equivalent to a secret-path-filtered file API.

Do not describe shell access as a sandbox unless a real sandbox is added.

## Mutation uncertainty is a first-class state

Remote systems fail in uncomfortable places:

```text
request sent
  ↓
mutation happened
  ↓
response lost
```

If the client blindly retries, the mutation may happen twice.

herdr-mcp therefore prefers:

- idempotency keys where available;
- explicit delivery evidence;
- transport failure separated from post-submit status waits;
- re-inspection before retrying uncertain mutation;
- state-based reconciliation for deployment/cutover operations.

This principle applies from agent prompts and shell execution to browser handoff and Cloudflare changes.

## Control-plane failure is not automatically project failure

Herdr snapshot/pane control can occasionally fail independently of the Git repository.

Read-only paths can degrade to narrower evidence sources, for example:

- list APIs instead of a full snapshot;
- direct Git facts;
- deterministic project file reads.

A Web planner should distinguish “I cannot currently inspect one control-plane object” from “the repository cannot be worked on.”

## Browser security boundary

The browser extension does not need the Herdr bearer in page JavaScript or service-worker storage.

Primary path:

```text
content script
  ↓
extension service worker
  ↓ Native Messaging
local host
  ↓ Unix socket (0600)
herdr-mcp runtime
```

Public ChatGPT access uses OAuth at Edge. Local browser continuity uses trusted local IPC. These are intentionally separate trust boundaries.

## Why this architecture is intentionally restrained

The system avoids creating duplicate layers:

- Herdr already manages agents and panes, so herdr-mcp does not create another agent registry;
- the Web AI already plans, so herdr-mcp does not create a workflow DSL;
- Git already provides source-of-truth state, so agent prose is not treated as completion evidence;
- Cloudflare already provides public routing/OAuth primitives, so the workstation does not expose itself directly.

The result is a control plane whose most important property is not the number of features, but the clarity of ownership.

## A typical repair loop

```text
Inspect live workspace
  ↓
Read Git + relevant files
  ↓
Make deterministic edits directly
  ↓
Delegate one narrow task only if useful
  ↓
Run tests / long command
  ↓
Use since + Git evidence
  ↓
Review
  ↓
Commit / deploy
  ↓
Browser continuity resumes the Web planner when needed
```

That is the architecture in practice: Web planning, persistent local execution, explicit evidence, and independently recoverable layers.

Related reading:

- [Design philosophy](design-philosophy.md)
- [Best practices](best-practices.md)
- [ChatGPT Connector](chatgpt-connector.md)
- [Browser continuity](browser-continuity.md)
- [Troubleshooting](troubleshooting.md)
