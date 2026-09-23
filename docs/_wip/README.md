# Active design work

`docs/_wip/` contains **active plans and design documents only**. Completed GA, release, migration, and acceptance evidence belongs under `docs/history/`.

Lifecycle:

- active design / implementation plan → keep here as Markdown;
- completed GA / release / migration evidence → move to `docs/history/`;
- reusable current product guidance → promote into `docs/i18n/<locale>/`, an ADR/current architecture document, or another maintained SSOT;
- one-off local data that may contain credentials or machine-specific state → keep outside the repository.

Current active WIP is limited to unresolved work: browser control-plane settlement, DEV/STANDALONE/STORE extension distribution, and modular progressive skills.

## Current index

- Stable 1.0 release: [`../releases/v1.0.0.md`](../releases/v1.0.0.md).
- 1.0 roadmap and milestone history: [`../herdr-architecture-roadmap.md`](../herdr-architecture-roadmap.md).
- Archived 1.0 closeout ledger: [`../history/architecture/v1.0-status-closeout-20260923.md`](../history/architecture/v1.0-status-closeout-20260923.md).
- Archived beta.2 orchestration design: [`../history/architecture/v1.0-beta2-webchat-orchestration.md`](../history/architecture/v1.0-beta2-webchat-orchestration.md).
- Archived 1.0 performance/resource plan: [`../history/architecture/v1.0-performance-resource-plan.md`](../history/architecture/v1.0-performance-resource-plan.md).
- Active browser control-plane work: [`browser-control-plane.md`](browser-control-plane.md).
- Active progressive-skills work: [`modular-progressive-skills.md`](modular-progressive-skills.md).
- Active browser-extension package/Store work: [`browser-extension-development-and-store-release.md`](browser-extension-development-and-store-release.md). Historical pre-1.0 Store rollout detail lives under [`../history/architecture/browser-extension-development-and-store-release-20260829.md`](../history/architecture/browser-extension-development-and-store-release-20260829.md).
