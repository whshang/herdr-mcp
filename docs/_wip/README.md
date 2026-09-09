# Active design work

`docs/_wip/` contains **active plans and design documents only**. It is not a dumping ground for completed UAT evidence, temporary recon output, release artifacts, or secrets.

Lifecycle:

- active design / implementation plan → keep here as Markdown;
- completed GA / release / migration evidence → move to `docs/history/`;
- reusable current product guidance → promote into `docs/i18n/<locale>/`, an ADR/current architecture document, or another maintained SSOT;
- one-off local data that may contain credentials or machine-specific state → keep outside the repository.

Current active WIP includes the v0.4.5 follow-up maintenance plan (planner efficiency plus onboarding/network resilience), browser control plane settlement, DEV/STANDALONE/STORE extension distribution work, and modular progressive skills. The completed v0.4.3 and v0.4.4 release plans, plus the multi-device core design, are archived under `docs/history/architecture/`. Completed release/qualification work must not remain an active-plan label. Remove or archive a plan when its implementation decision is complete; do not leave completed evidence here for convenience.

## v1.0 series index

- Frozen planning baseline (read-only SSOT for D1–D5, canonical WebChat resource model, typed operations, and §17 compatibility gates): [`../history/architecture/v1.0-architecture-plan.md`](../history/architecture/v1.0-architecture-plan.md), restored verbatim from `47a6f80:docs/_wip/v1.0-architecture-plan.md`.
- Milestone sequence authority: [`../herdr-architecture-roadmap.md`](../herdr-architecture-roadmap.md) (alpha.1 → alpha.2 → alpha.3 → alpha.4 → alpha.5 → beta.1 → beta.2 → rc.1).
- alpha.1 spec: [`v1.0-phase1-fleet-control-kernel.md`](v1.0-phase1-fleet-control-kernel.md)
- alpha.2 spec: [`v1.0-alpha2-work-memory.md`](v1.0-alpha2-work-memory.md)
- alpha.3 spec: [`v1.0-alpha3-browser-registry.md`](v1.0-alpha3-browser-registry.md) — merged in PR #315.
- alpha.4 spec: [`v1.0-alpha4-chatgpt-adapter.md`](v1.0-alpha4-chatgpt-adapter.md)
- alpha.5 spec: [`v1.0-alpha5-gemini-adapter.md`](v1.0-alpha5-gemini-adapter.md)
- alpha.6 spec: [`v1.0-alpha6-claude-adapter.md`](v1.0-alpha6-claude-adapter.md)
- alpha.8 spec: [`v1.0-alpha8-page-assist.md`](v1.0-alpha8-page-assist.md)
- alpha.9 spec: [`v1.0-toolchain-efficiency.md`](v1.0-toolchain-efficiency.md) — native-default exec and measured 18-tool efficiency gate before beta.1.
- Cross-cutting compile/memory efficiency plan: [`v1.0-performance-resource-plan.md`](v1.0-performance-resource-plan.md) — measurement-driven constraints applied inside the existing milestones; no new stage or scheduler.
- Stage progress ledger: [`v1.0-status.md`](v1.0-status.md) (engineering-stage progress only; release status remains owned by `docs/release-model.md`).
