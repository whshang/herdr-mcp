# Active design work

`docs/_wip/` contains **active plans and design documents only**. It is not a dumping ground for completed UAT evidence, temporary recon output, release artifacts, or secrets.

Lifecycle:

- active design / implementation plan → keep here as Markdown;
- completed GA / release / migration evidence → move to `docs/history/`;
- reusable current product guidance → promote into `docs/i18n/<locale>/`, an ADR/current architecture document, or another maintained SSOT;
- one-off local data that may contain credentials or machine-specific state → keep outside the repository.

Current active WIP includes the v0.4.5 follow-up maintenance plan (planner efficiency plus onboarding/network resilience), the v0.4.6 Work Memory + provider-neutral WebChat control architecture/interface as input to the consolidated Herdr-MCP 1.0 architecture plan, browser control plane settlement, DEV/STANDALONE/STORE extension distribution work, and modular progressive skills. `v1.0-architecture-plan.md` is the current cross-cutting decision and phased delivery plan for multi-controller WebChat, multi-device parallel development, Work Memory, provider-neutral Browser Control / External Conversation Dispatch, plugin seams, workstation portability, authentication and release gates. The generic browser endpoint/dispatch product belongs to 1.0; 0.4.6 remains limited to current-surface security, reliability, compatibility and regression fixes. `gitlab-ai-workflow` is an initial consumer and OpenCLI is research/UAT/fallback tooling, not a core or runtime dependency. The earlier ChatGPT-specific v0.4.6 dispatch plan is retained only as design/UAT evidence. The completed v0.4.3 and v0.4.4 release plans, plus the multi-device core design, are archived under `docs/history/architecture/`. Completed release/qualification work must not remain an active-plan label. Remove or archive a plan when its implementation decision is complete; do not leave completed evidence here for convenience.
