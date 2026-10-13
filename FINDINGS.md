# Findings

Lessons from daily commit reviews. Each entry uses the template below. When the
same lesson is seen on 3 different review days, move it to RULES.md.

Template:

**<one-line rule>**
- <which commit / issue caused it, and the consequence>
- <fix commit>
- Done right: <example done right>
- Seen N× (YYYY-MM-DD, YYYY-MM-DD).

---

**A release that must not reach the stable channel needs a prerelease-suffixed version, not only the GitHub prerelease flag.**
- v1.0.4 was tagged from 0c840628 (#606) with version `1.0.4`; the release was then flagged prerelease on GitHub, but the stable updater selects by semver only. `herdr-mcp update check` on a 1.0.1 PROD machine (2026-10-13) reported `tag v1.0.4`, `update_channel stable`, so stable installs can pick it up.
- Fix: none needed for v1.0.4 itself; on 2026-10-13 asu accepted v1.0.4 as stable after PROD UAT (1.0.1 → 1.0.4 → rollback → 1.0.4) and it was promoted to latest. Future UAT-only builds must use `-rc.N` versions.
- Done right: v1.1.0-rc.1 (#604) used `1.1.0-rc.1`; the workflow adds `--prerelease` for `*-*` tags and the stable channel rejects it.
- Seen 1× (2026-10-12).

**Release notes must not claim an upgrade path that was not exercised end to end.**
- #604 (v1.1.0-rc.1 notes) said 1.0.x could update directly to rc.1; released 1.0.x updaters refuse it with `release manifest contract identity mismatch` (epoch 4 vs 5), found during PROD UAT.
- Fix: #607 (b4ce6550) docs, plus the corrected rc.1 Release page; bridge path via #606 / #605.
- Done right: v1.0.4 notes describe 1.0.0–1.0.3 → 1.0.4, and that path passed real UAT on a 1.0.1 PROD machine (2026-10-13).
- Seen 1× (2026-10-12).

**A newer runtime must never write a persisted value that an older runtime reads as a wider permission after rollback.**
- e44688a0 (on main) made registration insert `webchat_control_allowed = 1` and re-registration overwrite a stored `0`; epoch 4 reads `!= 0` as allowed, so rollback widened permissions. Found by the cross-audit of #605.
- Fix: 3e68870d (#605, merged d509481f).
- Done right: re-registration preserves the stored value and new rows default to 0, with rollback-read tests.
- Seen 1× (2026-10-12).

**Never edit an applied migration; add a new migration or a code-path change.**
- 3e68870d edited the text of applied `MIGRATION_V7`, against the module's own rule; caught by audit before merge.
- Fix: 719d8028 (restored byte-identical text plus a hash pin test).
- Done right: 719d8028 keeps the migration immutable and moves the guarantee to the registration writers.
- Seen 1× (2026-10-12).
