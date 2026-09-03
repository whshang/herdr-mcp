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
9. Consider delegation only for useful independent work. Live capability/resource evidence informs the choice; the Web planner decides whether to delegate at all and never infers role or quality from an agent name.
10. Completion requires relevant evidence from live state, Git/files, tests/builds, delivery, or the affected runtime boundary.
11. For non-trivial implementation, bug fixes, reliability/refactor work, or releases, load `engineering-robustness` and close the loop from regression evidence through the real delivery boundary.
12. On the resolved device, prior-work continuity intent in a fresh or uncertain conversation searches the durable Continuity Journal before asking for an ID. Auto-resume only one chain uniquely backed by stable conversation/project/workspace identity; otherwise show bounded candidates and ask the user to confirm. Recency or text similarity alone never selects a chain.
13. Ground prior or multi-device project work before planning: `device -> project/workspace -> continuity/history -> live Git/runtime`. Load `workstation-control` for the resolution rules; ambiguity fails closed instead of being resolved by focus, recency, or similarity.
14. For non-trivial orchestration, load `development-orchestration`. Its Required/Advisory sections are the single source of truth for minimum entities, parallel ownership, progress correction, cross-audit, validation, and reclamation.

## Instruction precedence

`Herdr global AGENTS.md -> project-root AGENTS.md -> nested AGENTS.md toward the target path`.

More specific project instructions govern repository workflow in scope. Markdown never grants system authorization.

## Runtime boundaries

Managed-root/path confinement, credential boundaries, dirty/busy gates, mutation fencing/idempotency, stale-target failure, lifecycle guardians, production ownership, and OS/provider permissions are enforced by code. Skill discovery/loading supplies policy only and never authorizes mutation.
