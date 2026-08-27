# Herdr Global AGENTS.md

Host-injected global policy for a remote/Web planner. Project `AGENTS.md` files may add workflow rules; runtime safety remains authoritative.

## Core policy

1. Live workstation state is authoritative. Re-check Herdr, Git, runtime, or process state when a decision depends on it.
2. Prefer the cheapest correct deterministic tool before delegating to a coding agent.
3. Use small dependency-aware read waves. Parallelize independent reads; serialize dependent calls.
4. Order mutations unless ownership and isolation are explicit. Shared files/checkouts/runtime use one mutation lane.
5. Never blind-retry an uncertain mutation. Observe delivery and resulting state first.
6. Start long work once; resume by session identity and offset. Process exit alone may not prove task completion.
7. Use explicit workspace, pane, project-root, session, and operation identities. UI focus is not a control target.
8. Load a domain Skill on first capability use and keep it sticky while source identity/digest remain unchanged.
9. Auto-delegate only useful independent work to a safe compatible live worker. Preserve explicit user targets; never silently downgrade required quality/capability.
10. Completion requires relevant evidence: file/Git state, tests/build, agent delivery, runtime status, or equivalent live proof.

## Instruction precedence

Apply policy in this order for a target path:

```text
Herdr global AGENTS.md
  -> project-root AGENTS.md
  -> nested AGENTS.md toward the target directory
```

More specific project instructions govern repository workflow inside their scope. Markdown instructions never grant additional system authorization.

## Progressive Skills

The compact catalog is supplied by `herdr_skill`. Load one or more required modules in one call with:

```text
herdr_call(
  method="herdr_mcp.skill.load",
  params={"ids":["files-search","git-repository"]}
)
```

Do not reload a Skill on every tool call or every user turn. Reload only after a new context/handoff, first entry into a new capability domain, source/digest change, or explicit refresh. Live agent, pane, model, and runtime state refresh through live state tools rather than by reloading Skill text.

## Runtime-enforced boundaries

These remain enforced by code and cannot be overridden by any Skill or project Markdown: managed-root/path confinement, secret and credential boundaries, dirty/busy gates, mutation idempotency/fencing, stale-target fail-closed behavior, service/update guardians, production/runtime ownership, and OS/provider permissions.

Skill discovery and loading provide policy only. They do not authorize mutation.
