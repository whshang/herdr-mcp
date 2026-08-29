# GA closure evidence archive

**Completed / Historical** — these files preserve GA sprint evidence. They do **not** replace the live scorecard in [`../../ga-release-gate.md`](../../ga-release-gate.md).

| File | Topic | Status |
| --- | --- | --- |
| [`g4-second-mac-stable-v040-uat-20260828.md`](./g4-second-mac-stable-v040-uat-20260828.md) | G4 second Mac clean install from `v0.4.0` stable Release | **PASS** (pi-ga-20260828) |
| [`g18-clean-machine-sim-20260828.md`](./g18-clean-machine-sim-20260828.md) | Same-Mac TMPHOME simulation (not second machine) | Historical PARTIAL |
| [`g5-link-production-cutover.md`](./g5-link-production-cutover.md) | G5 Link prod cutover runbook (executed 2026-08-27) | Historical — production Link Rust |
| [`g20-command-contract.md`](./g20-command-contract.md) | G20 CLI ↔ docs command contract audit notes | Historical reference |
| [`exit-alpha-checklist.md`](./exit-alpha-checklist.md) | G1 exit-alpha unification runbook (`v0.4.0` stable) | Historical — archived from `docs/` |
| [`g67-dogfood-public-uat-20260828.json`](./g67-dogfood-public-uat-20260828.json) | G6/G7 public MCP dogfood evidence | **PASS** — archived machine evidence |
| [`g67-dogfood-public-uat-20260828.mjs`](./g67-dogfood-public-uat-20260828.mjs) | Reproducible G6/G7 OAuth/MCP UAT harness; obtains tokens at runtime and does not contain a stored credential | Historical harness |
| [`g910-rc1-rehearsal-20260828.json`](./g910-rc1-rehearsal-20260828.json) | Early G9/G10 rc.1 rehearsal record | Historical rehearsal |
| [`g910-rc1-stable-rehearsal-20260828.json`](./g910-rc1-stable-rehearsal-20260828.json) | G9/G10 preview/stable rehearsal record | **PASS** — archived evidence |
| [`second-mac-ga-uat-agent-prompt-en.md`](./second-mac-ga-uat-agent-prompt-en.md) | Internal UAT agent protocol (English) | Completed — G4 sealed |
| [`second-mac-ga-uat-agent-prompt-zh-CN.md`](./second-mac-ga-uat-agent-prompt-zh-CN.md) | Internal UAT agent protocol (Chinese) | Completed — G4 sealed |

Live operator prompts for **future** UAT cycles should fork from [`../../i18n/en/clean-machine-uat.md`](../../i18n/en/clean-machine-uat.md), not resurrect alpha-era prompts without rescoping.
