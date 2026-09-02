# Active design work

`docs/_wip/` contains **active plans and design documents only**. It is not a dumping ground for completed UAT evidence, temporary recon output, release artifacts, or secrets.

Lifecycle:

- active design / implementation plan → keep here as Markdown;
- completed GA / release / migration evidence → move to `docs/history/`;
- reusable current product guidance → promote into `docs/i18n/<locale>/`, an ADR/current architecture document, or another maintained SSOT;
- one-off local data that may contain credentials or machine-specific state → keep outside the repository.

Current active WIP includes the v0.4.5 follow-up maintenance plan, browser control plane settlement, DEV/STANDALONE/STORE extension distribution work, and modular progressive skills. The completed v0.4.3 and v0.4.4 release plans, plus the multi-device core design, are archived under `docs/history/architecture/`. Completed release/qualification work must not remain an active-plan label. Remove or archive a plan when its implementation decision is complete; do not leave completed evidence here for convenience.
