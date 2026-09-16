# Active design work

`docs/_wip/` contains **active plans and design documents only**. It is not a dumping ground for completed UAT evidence, temporary recon output, release artifacts, or secrets.

Lifecycle:

- active design / implementation plan → keep here as Markdown;
- completed GA / release / migration evidence → move to `docs/history/`;
- reusable current product guidance → promote into `docs/i18n/<locale>/`, an ADR/current architecture document, or another maintained SSOT;
- one-off local data that may contain credentials or machine-specific state → keep outside the repository.

Current active WIP is limited to work that still has an unresolved implementation or acceptance boundary: browser control-plane settlement, DEV/STANDALONE/STORE extension distribution, modular progressive skills, beta.2 signed-in/cross-provider acceptance, the cross-cutting performance/resource plan, and the current 1.0 status ledger. Completed release and implementation records live under `docs/history/`; do not keep them active merely for convenient lookup.

## v1.0 series index

- Frozen planning baseline: [`../history/architecture/v1.0-architecture-plan.md`](../history/architecture/v1.0-architecture-plan.md).
- Milestone sequence authority: [`../herdr-architecture-roadmap.md`](../herdr-architecture-roadmap.md).
- Current stage/status SSOT: [`v1.0-status.md`](v1.0-status.md).
- Active beta.2 acceptance/design boundary: [`v1.0-beta2-webchat-orchestration.md`](v1.0-beta2-webchat-orchestration.md).
- Active compile/memory/resource plan: [`v1.0-performance-resource-plan.md`](v1.0-performance-resource-plan.md).
- Completed alpha.1–alpha.9 and beta.1 implementation records: [`../history/architecture/`](../history/architecture/).
- Mainline convergence / branch-retirement closeout: [`../history/architecture/v1.0-mainline-convergence-closeout-20260913-14.md`](../history/architecture/v1.0-mainline-convergence-closeout-20260913-14.md).
