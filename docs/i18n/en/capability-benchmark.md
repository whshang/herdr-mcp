# Capability benchmark and design choices: what to absorb, what not to copy

This page is for maintainers and contributors. It is not a feature-count comparison. It is a long-lived ADR for deciding whether a capability from Herdr, another MCP implementation, a coding-agent framework or a browser integration belongs in herdr-mcp.

The question is:

> Does this capability make a Web AI more reliable at controlling a local development environment without duplicating Herdr, the agent runtime or another system that already owns the problem?

## Define the boundary first

herdr-mcp is not:

- a second Herdr;
- another coding agent;
- a generic remote-shell product;
- a workflow/recipe DSL;
- a general browser-automation framework.

It is the control plane between a Web AI and a local Herdr/workstation. The highest-value first-class capabilities are therefore:

1. workstation capabilities the Web model cannot otherwise reach;
2. persistent state across long tasks and conversations;
3. a stable and secure path from the public Web to a private workstation;
4. observable mutation, delivery and recovery semantics.

## A decision filter

When evaluating a new capability, ask:

```text
Does the Web planner actually lack it?
  ↓ yes
Does Herdr already expose it natively?
  ↓ yes → discover/passthrough, do not duplicate
  ↓ no
Can existing fs/git/exec primitives express it?
  ↓ yes → reuse them
  ↓ no
Would a dedicated capability materially improve reliability?
  ↓ yes → consider a stable public surface
```

This matters more than “another project has a tool for it.”

## Choice 1: fixed public MCP surface, dynamic Herdr long tail

Herdr's native Socket API is broad and evolving. Registering every `workspace.*`, `pane.*` and `agent.*` method as a public MCP tool would:

- inflate the schema carried into each conversation;
- couple Herdr upgrades to the ChatGPT public ABI.

So herdr-mcp uses two layers:

```text
high-frequency remote work
  → dedicated MCP tools

long-tail native Herdr operations
  → herdr_methods + herdr_call
```

The current production contract is **epoch 2 / 18 tools**. Future catalog changes require explicit contract epochs rather than incidental runtime changes.

## Choice 2: files, Git and shell are first-class

These are not Herdr's responsibility, but they are exactly what a remote Web model cannot access by itself.

First-class capabilities include:

- file read/list/search/image;
- exact edit/write/patch;
- Git status/diff/log;
- short shell commands;
- long-running command sessions.

Deterministic repository work should not require launching another model.

## Choice 3: long commands have their own lifecycle

A build, test or development server may outlive one MCP request. Binding command lifetime to a synchronous HTTP request creates timeout and duplicate-execution risk.

So:

```text
short command
  → herdr_exec

long command
  → herdr_exec_start
        ↓
     read / kill
```

A handle-based command lifecycle solves a real remote-execution problem without inventing another agent abstraction.

## Choice 4: Git facts remain deterministic

`git status`, `git diff` and `git log` do not benefit from an extra model in the loop.

`herdr_git` exists because direct facts are:

- cheaper;
- faster;
- easier to verify;
- useful as mutation-completion evidence.

Low-frequency Git commands can still go through shell. A dedicated public tool is justified only when stable schema and frequent use are worth the added surface.

## Choice 5: mutation semantics matter more than automatic retry

The dangerous remote failure is:

```text
mutation happened
  ↓
response was lost
```

So herdr-mcp favors:

- delivery evidence;
- idempotency keys;
- transport failure separated from post-submit waiting;
- state inspection before retrying uncertain mutation.

This applies to agent prompts, shell commands, runtime activation, Cloudflare/DNS changes and browser handoff.

“Retry on error” is not a safe generic policy for a development control plane.

## Choice 6: shell is not presented as a sandbox

Some systems classify commands as safe/trusted/dangerous and can give the impression of strong isolation.

herdr-mcp keeps the real boundary explicit:

- `herdr_fs_*` is constrained by managed roots, write gates and secret-ish path filtering;
- `herdr_exec` runs as the workstation user and is a stronger capability.

If container-grade isolation is needed, it should be designed as an actual security architecture rather than implied by a flag.

## Choice 7: do not duplicate project-instruction systems

Coding agents already have their own project instructions, skills or `AGENTS.md`-style mechanisms.

herdr-mcp does not build another automatic project-instruction scanner because that would create:

- duplicated context;
- priority conflicts;
- inconsistent rules between the Web planner and local workers.

Remote-planner operating policy belongs in `herdr_skill`; project-specific rules remain owned by the project and the agent runtime actually executing them.

## Choice 8: the browser extension only fills missing directions

Request-driven MCP provides:

```text
Web AI → workstation
```

It does not make a finished local task start a new browser turn later.

The extension therefore provides the missing time/return direction:

```text
workstation → browser conversation
```

Worthwhile extension capabilities include:

- workspace binding;
- progress / settled;
- evidence-first recovery;
- fail-closed handoff;
- a bounded JSON→MCP bridge for sites without a native Connector.

The project intentionally does not expand that into general-purpose browser automation.

## Choice 9: public Edge and local runtime are decoupled

A Connector URL should be stable while a local runtime should be upgradeable.

That leads to:

- Cloudflare Edge for OAuth/public MCP/workstation routing;
- `herdr-link` for the persistent outbound WSS;
- Runtime A/B for local generation changes.

This is more structured than tunneling directly to one Node process, but it allows runtime upgrade/rollback without changing the public identity.

## Choice 10: local agents are workers, not another planner

Pi, Cline, OpenCode, DSH or future coding-agent CLIs can all be useful workers.

The durable selection criteria are:

- headless/automatable operation;
- observable state;
- bounded task ownership;
- verifiable results;
- the ability to determine whether mutation occurred after timeout.

Herdr-native workers use `herdr_prompt`; external CLIs can use long exec sessions when appropriate.

See [Worker fallbacks](worker-fallbacks.md).

## Current decision matrix

| Capability | Decision | Why |
|---|---|---|
| Herdr workspace/pane/agent API | dynamic passthrough | avoid copying the native API surface |
| file read/search/patch | first-class MCP | otherwise unreachable to the Web planner |
| Git status/diff/log | first-class MCP | frequent deterministic evidence |
| long command session | first-class MCP | lifecycle crosses tool calls |
| image read | first-class MCP | real pixel context for the Web model |
| agent prompt | thin wrapper | delivery/idempotency semantics matter |
| recipe/workflow DSL | do not build | Web AI is already the planner |
| second agent registry | do not build | Herdr already owns it |
| automatic project-instruction scanning | do not build | avoid duplicating agent-runtime policy |
| shell pseudo-sandbox | do not claim | security boundaries must be real |
| browser progress/recovery/handoff | build | fills MCP's temporal/return gap |
| JSON→MCP bridge | bounded compatibility | only for sites without native custom MCP |
| Runtime A/B | build | decouples Connector identity from local upgrades |
| Custom Domain | optional | stable naming, not a product prerequisite |

## Admission rules for a new capability

Before adding a public tool or automation module, answer:

1. Can `herdr_call` already express it?
2. Can existing fs/git/exec primitives express it?
3. Why does it need a stable public schema?
4. What is the per-turn context cost?
5. How do we know whether a mutation already happened?
6. How does failure recover?
7. Can it be tested against real behavior rather than only a wrapper unit test?
8. Are we duplicating Herdr, the agent runtime or Cloudflare?

If those answers are unclear, do not expand the surface yet.

## How to maintain this page

This is not an experiment log.

When an upstream tool or project introduces a new capability, update the **decision** here. Put exact versions, smoke-test dates, one-off UAT results and bug evidence in CHANGELOG, issues or experiment records.

That keeps this page useful for the question that matters over time:

> Why is herdr-mcp shaped this way, and is the next capability actually worth bringing inside its boundary?
