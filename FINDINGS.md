# Findings

Lessons from daily commit reviews. Each entry uses the template below. When the
same lesson is recorded on 3 distinct review dates, move it to RULES.md
(see the maintenance rules in AGENTS.md). `Seen N×` counts distinct review dates.

Template:

**<one-line rule>**
- <which commit / issue caused it, and the consequence>
- <fix commit>
- Done right: <example done right>
- Seen N× (YYYY-MM-DD, YYYY-MM-DD).

---

**A build that must not reach the stable channel needs a prerelease-suffixed version (`-rc.N`), not only the GitHub prerelease flag.**
- v1.0.4 was tagged from 0c840628 (#606) with version `1.0.4`; the release was then flagged prerelease on GitHub, but the stable updater selects by semver only. `herdr-mcp update check` on a 1.0.1 PROD machine (2026-10-13) reported `tag v1.0.4`, `update_channel stable`, so stable installs can pick it up.
- Fix: no code fix. v1.0.4 is an accepted exception: on 2026-10-13 asu accepted it as stable after PROD UAT (1.0.1 → 1.0.4 → rollback → 1.0.4) and it was promoted to latest. The rule stands for future UAT-only builds.
- Done right: v1.1.0-rc.1 (#604) used `1.1.0-rc.1`; the workflow adds `--prerelease` for `*-*` tags and the stable channel rejects it.
- Seen 1× (2026-10-13).

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

**Cross-layer action parameters must be verified from the real native-entry hop; per-layer unit passes do not prove the bridge forwards them.**
- #592 (7a913e2b / db142028) added `press` and `extract_table`, but `handleBrowserActuation` omitted `key` / `max_rows` on the background hop. DEV UAT on extension 0.1.199 (PR comment 6139867459) saw every press `not_applied` (`press_params_invalid`) and `max_rows:1` returning 3 rows. Same class as 369ba686 (scroll direction/amount dropped).
- Fix: 313371b5 (merged in #592 / b7ffbb1c) forwards the params and adds two native-entry regression tests that fail without the passthrough.
- Done right: after 313371b5, DEV UAT on 0.1.200 recorded press Escape applied and `max_rows:1` returning one row with `rows_truncated=true`; merge body b7ffbb1c states rollback (remove passthrough → 2 tests fail) and real-usage results.
- Seen 1× (2026-10-13).

**Before lengthening one wait, recompute the full-chain budget and separate slow-load timeouts from registration self-cancellation.**
- #620: c72ea013 raised the create-scope handshake from 8s to 30s; that made the worst-case create path ~73s and exceeded the 53s Rust broker wait (074703c5 body: "Follow-up to c72ea013"). Separately, newest-attempt-wins plus a ~2.5s pre-`h2w_register` window longer than the poll gap meant `h2w_register` was never sent (`registry_handshake_delay`); more waiting alone could not fix that.
- Fix: 074703c5 (broker 80s / CLI 115s) and 3972d566 (per-conversation single-flight registration, refresh chaining, abort/cache guards), merged in #620 / 07c91731.
- Done right: 07c91731 merge body records two real ChatGPT `webchat create` calls with `delivery_state=applied`, mutation tests for handshake/broker/single-flight pieces, and explicitly lists NOT run (slow-proxy, Edge/Link >60s, macOS/Windows, PROD).
- Seen 1× (2026-10-13).

**When a Skill reference file changes, update its `manifest.json` digest in the same change.**
- #625: 47267e30 edited `references/browser-control.md` without updating the Skill manifest hash (AGENTS.md already requires same-change digest updates). Caught same day before merge.
- Fix: 6f118e50 updates the browser-control.md SHA-256 in the agent Skill manifest (merged in #625 / caf1584f).
- Done right: 6f118e50 is a one-line digest sync committed before the merge; keep doc + hash in the same diff next time.
- Seen 1× (2026-10-13).

**A harness fake reply proves the relay contract only; do not name or treat it as live runtime enforcement.**
- #611: cb4d4256 initially framed the Edge no-grant stop case as the user seeing "the runtime fail-closed refusal" while the refusal was a fake device reply and an undefined trace could still satisfy a "no grants" assertion.
- Fix: 61ee9b99 renames the case to "runtime refusal relayed unchanged", requires a forwarded trace, and comments that runtime enforcement is pinned by the Rust test; merge body 05d83989 discloses real-usage not done and no true Edge-to-runtime integration test.
- Done right: 05d83989 / 61ee9b99 keep relay-only and Rust fail-closed scopes explicit, with NOT run listed.
- Seen 1× (2026-10-13).

**The acceptance trio (rollback, real-usage, NOT run) must appear in the landing merge commit body and in an acceptance PR comment—not only in a feature commit or the PR description.**
- After #609 landed the AGENTS.md rule, merge commits #609 (619e254a), #625 (caf1584f), and #627 (d193380a) carried only short subject lines; #625/#627 had empty PR comment threads. Feature/PR bodies had partial or full statements that did not transfer to the landing commit.
- Fix: none yet (docs process). Recorded by daily retro 2026-10-13.
- Done right: #591 (91bf2aae), #611 (05d83989), and #620 (07c91731) merge bodies each state rollback, real-usage, and NOT run; #592 also left acceptance evidence in PR comments.
- Seen 1× (2026-10-13).
