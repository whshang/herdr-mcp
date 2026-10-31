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
- #604 (v1.1.0-rc.1 notes) said 1.0.x could update directly to rc.1; released 1.0.x updaters refuse it with `release manifest contract identity mismatch` (epoch 4 vs 5), found during PROD UAT. Same lesson on 2026-10-30: #761 / `docs/releases/v1.1.0-rc.6.md` still names the 1.0.x path as `v1.0.x → v1.0.4 bridge → v1.1.0-rc.6`, but shipped v1.0.4 refuses every schema-16 manifest (#763 body known limitation); the working bridge is v1.0.5 (#783 / #779).
- Fix: #607 (b4ce6550) docs, plus the corrected rc.1 Release page; bridge path via #606 / #605. For rc.6 the updater bridge landed as #783 (d9d1821c on `release/v1.0.x`), but the rc.6 notes still say v1.0.4 — notes need to name v1.0.5.
- Done right: v1.0.4 notes describe 1.0.0–1.0.3 → 1.0.4, and that path passed real UAT on a 1.0.1 PROD machine (2026-10-13). v1.0.5 notes correctly describe themselves as the schema-migration bridge and that v1.0.4 cannot install schema-16.
- Seen 2× (2026-10-12, 2026-10-30).

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

**Do not merge a change that still edits a published or tagged release note after a review asked for that edit to move.**
- #712 squash 80b3cc24 (2026-10-27) still added a current "Agent 编排与上下文控制" section and a current known-issue line to `docs/releases/v1.1.0.md`. Comment 6438452465 (CHANGES on tip 9047cac6, the commit the merge body cites as PASS-WITH-NITS) said that file is the frozen tagged note and asked to move the material to `docs/releases/1.1-feature-list.md`. The feature list already says the tagged notes stay frozen in `v1.1.0.md`. A same-minute APPROVE (6438460913) did not remove the edit, and the squash landed it. `docs/releases/1.1-feature-list.md` is the living list; `v1.1.0.md` is the authored note a `v1.1.0` tag would publish.
- Fix: none yet. Move those two additions out of `docs/releases/v1.1.0.md`.
- Done right: the same squash also added the historical RC3 block to `docs/releases/1.1-feature-list.md`, which is where post-tag qualification belongs.
- Seen 1× (2026-10-27).

**When retiring a persisted locator, migrate or delete dependent evidence in the same transaction.**
- #743 / 46e4d1b5 (2026-10-29) retired the old-parent ChatGPT locator on reparent but left its `browser_source_turns` row behind. A turn observed before the reparent and re-observed after sat on two sessions with the same digest; resolution stayed `source_turn_ambiguous` until expiry. Codex review after merge called this BLOCKING: the new test only recorded the turn *after* reparenting, so the prior-evidence path was unproven. Same-day follow-up #744 / a0f6ea4f moved the retired session's latest source turn onto the surviving session (newer observation wins) and deleted the old row in one transaction.
- Fix: a0f6ea4f (#744).
- Done right: #744 / a0f6ea4f carries source-turn evidence with locator retirement; `reparented_conversation_with_prior_source_turn_stays_unambiguous` fails on main without that change, and the merge body states the Codex #743 gap.
- Seen 1× (2026-10-29).

**Source-turn binding verification must cover every live ChatGPT chip rendering of the same mention, not only the mention-prefix forms listed in the issue.**
- #760 / bd8866f3 (#759) stripped leading `@`/`$`/bare/fenced mentions and passed real SQLite tests for those forms; same-day #777 UAT still got `source_turn_not_found` when the model re-rendered the app chip (markdown link, display label, invisible controls). Consequence: #780 / 5d048ccf had to widen the same canonicalizer again the same day. Same-day #755 also needed a second commit after live MBP DOM showed the accepted bubble uses `data-prompt-link-href` chips the first fix missed.
- Fix: 5d048ccf (#780); 3ddf3b09 second commit (#755).
- Done right: #780 included the #777 UAT digest `0131a3e87a63` for every listed chip rendering and an end-to-end observe-vs-dispatch bind for the markdown-link rendering.
- Seen 1× (2026-10-30).
