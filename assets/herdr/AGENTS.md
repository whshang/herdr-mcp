# Herdr Global AGENTS.md

Global policy for a remote/Web planner. Project and nested `AGENTS.md` files may refine repository workflow; runtime safety remains authoritative.

## Core policy

1. Live workstation state is authoritative. Re-check state when a decision depends on it.
2. Prefer the cheapest correct deterministic tool before delegating.
3. Group independent reads into small dependency-aware waves; serialize dependent work.
4. Keep mutations ordered unless ownership and isolation are explicit. Shared files, checkout, or runtime use one mutation lane.
5. Never blind-retry an uncertain mutation. Observe delivery and resulting state first.
6. Start long work once and resume by session identity/offset. Process exit alone may not prove task completion.
7. Use explicit workspace, pane, project-root, session, and operation identities. UI focus is not a control target.
8. Load domain Skills only when required and keep them sticky while source identity/digest are unchanged.
9. Auto-delegate only useful independent work to a safe compatible live worker. Preserve explicit user targets and required quality.
10. Completion requires relevant evidence from live state, Git/files, tests/builds, delivery, or the affected runtime boundary.
11. For non-trivial implementation, bug fixes, reliability/refactor work, or releases, load `engineering-robustness` and close the loop from regression evidence through the real delivery boundary.

## Instruction precedence

`Herdr global AGENTS.md -> project-root AGENTS.md -> nested AGENTS.md toward the target path`.

More specific project instructions govern repository workflow in scope. Markdown never grants system authorization.

## Runtime boundaries

Managed-root/path confinement, credential boundaries, dirty/busy gates, mutation fencing/idempotency, stale-target failure, lifecycle guardians, production ownership, and OS/provider permissions are enforced by code. Skill discovery/loading supplies policy only and never authorizes mutation.
