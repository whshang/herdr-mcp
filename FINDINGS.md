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
- Fix: #607 (b4ce6550) docs, plus the corrected rc.1 Release page; bridge path via #606 / #605. The 2026-10-30 rc.6 notes gap remains in `docs/releases/v1.1.0-rc.6.md` (updater bridge is #783 / d9d1821c on `release/v1.0.x`). Living draft `docs/releases/v1.1.0.md` was corrected by #778 (548af203) to name the v1.0.5 bridge. #789 `docs/releases/v1.1.0-rc.7.md` names v1.0.4 → v1.0.5 → v1.1.0 and keeps this RC on preview.
- Done right: v1.0.4 notes describe 1.0.0–1.0.3 → 1.0.4, and that path passed real UAT on a 1.0.1 PROD machine (2026-10-13). v1.0.5 notes describe themselves as the schema-migration bridge and state that v1.0.4 cannot install schema-16. After #778, `docs/releases/v1.1.0.md` names v1.0.5 and says the two-step path waits for the formal release.
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

**Do not treat missing local doctor evidence as an authoritative Edge WebChat grant absence.**
- #772 / da451e61 (merged #782 / e11ab407 on 2026-10-30) printed `WebChat grants: state=absent` and "extension consent gates browser control" whenever the local state DB had no connector-attributed dispatch evidence. This runtime cannot read Edge grants (`POST /connectors/webchat-control` is write-only), and `webchat_control_allowed` was retired in e44688a0, so a stale `0` row is not a gate. #728 UAT followed that hint. Same-week #793 / fc00c83f reports `state=absent_locally` with `authoritative=false` and drops the retired consent sentence.
- Fix: fc00c83f (#793). The pre-merge follow-up on 1fbc5706 stops the rollup from predicting `grant_missing`; it is the same landing.
- Done right: fc00c83f and comment 6555229273. A temp DB seeded with the #728 row (`bep_614dc378ca5b`, `webchat_control_allowed=0`, no dispatch rows) prints `state=absent_locally`, `webchat_control=legacy_ignored`, and the connector `allow` command. Live Edge read is listed under Not run.
- Seen 1× (2026-11-01).

**A refused `browser_session.create` must project an allowlisted extension rejection code, not `reason: null`.**
- #706 / 92de55c3 (2026-10-27) sanitized first-attempt `session.create` error evidence and dropped the extension's `result.error`, so every refused create returned `code: rejected, reason: null`. #728 UAT hid four `browser_create_composer_busy` refusals from the caller. Same-week #794 / af0b55a5 projects a fixed allowlist as `reason` and `delivery_evidence.error`, forwards a bounded `composer_busy_probe` for the busy code, and still drops any other string.
- Fix: af0b55a5 (#794). The pre-merge follow-up on a5f93095 mirrors that `delivery_evidence` on the reconcile fallthrough; it is the same landing.
- Done right: af0b55a5 and comment 6555295952. Scripted-actuator tests replay the extension evidence shape. Live browser UAT is listed under NOT run.
- Seen 1× (2026-11-01).

**An unattended `update apply` must not start an interactive Cloudflare or browser login.**
- 3561a7f5 (2026-09-16) chained a fleet Worker update after a successful runtime `update apply`. That path opens a Cloudflare device login and a browser. #796: a detached PROD run blocked for minutes and ended `update_succeeded_worker_update_required`. #797 / 02685b8c keeps `update apply` runtime-only, reports Worker drift as non-fatal `worker_update_required`, and refuses `update apply --with-worker` unless stdin and stdout are interactive TTYs (`with_worker_requires_interactive_tty`) before any update work. Same lesson on 2026-11-02: #828, a MacBook Air stable-channel `update apply --manifest` from v1.1.0-rc.6 to rc.11, still returned `update_succeeded_worker_update_required` with `ok: false` / exit 1 after the Runtime health gate passed. That payload is the pre-#797 updater (`updater.rs` in v1.1.0-rc.6..rc.8). The durable job was the Runtime success. #830 / 5b3bcdfb audits current base c447022a as RuntimeOnly unless `--with-worker` is requested from an interactive terminal, and teaches `update status` to report a legacy chained-Worker job as Runtime success (`runtime_outcome: succeeded`, notice `worker_update_failed_legacy`) when the installed version already equals the job target.
- Fix: 02685b8c (#797) stops the unattended chain. 5b3bcdfb (#830) reports the legacy job as that Runtime success when the swap committed; a job whose target is not installed stays failed. The pre-merge Windows `#[cfg]` restore in the #797 squash only puts back the gate the first commit on that branch removed. It is not a separate lesson.
- Done right: 02685b8c. Non-interactive `--with-worker` exits 1 with `with_worker_requires_interactive_tty` and does not start wrangler. The squash body states the trio. The PR has no acceptance comment. On 2026-11-02, 5b3bcdfb's `update_apply_with_manifest_on_stable_never_invokes_worker_update` and `update_status_reports_a_legacy_failed_job_as_success_when_the_swap_committed` cover the stable `--manifest` path and the legacy record. The #830 landing body does not state the trio.
- Seen 2× (2026-11-01, 2026-11-02).

**A runtime `update apply` must stage the matching standalone extension (and doctor must detect version/content drift), not only swap the runtime binary.**
- #833 / #838 (6ccd3ea4): MBP PROD ran runtime 1.1.0-rc.11 while `extensions/standalone/current` was still the copy staged on 2026-10-30 from 23a3421c. `update apply` never re-staged that tree. Both builds were manifest 0.1.229, and doctor only checked the manifest key, so the drift was invisible and the rc.11 extension fixes were not loaded.
- Fix: 6ccd3ea4 (#838, includes #833). `update apply` re-stages the managed standalone copy from the release it installs when a standalone install already exists; a staging failure is a non-fatal notice. The manifest version moves (0.1.229 → 0.1.230, then 0.1.240 in the same squash) and `scripts/check-extension-version.mjs` fails a release that changes `extension/` without a version bump. Doctor `standalone-extension-version` reports `reload_pending`, `extension_stale`, `extension_ahead`, or `unknown`. The same squash creates the managed tree mode 0700/0600. 2026-11-10 follow-up, not a recurrence: #885 / #898 (c94fdebda971) keeps `extension_stale` for a version or source-manifest SHA mismatch and reports `current` with reason `source_commit_advanced_extension_unchanged` when the runtime commit advanced and the staged extension version and manifest SHA still match (`standalone_extension.rs` `content_stale`). It does not change `update apply` staging.
- Done right: 6ccd3ea4. `staging_keeps_the_previous_tree_when_a_download_fails` leaves the previous tree when a download fails. The squash body records `doctor --verbose` against the real `~/.config` printing `state=extension_stale`, and lists live `update apply` under NOT run.
- Seen 1× (2026-11-02).

**Do not merge or tag a prerelease whose authored notes fail validate-release-notes; a later commit on main does not repair the tagged tree.**
- #853 / 6b2a8886 (2026-11-03) merged `docs/releases/v1.1.0-rc.14.md` without a Compatibility section. Tag `v1.1.0-rc.14` peels to that commit. Rust Release run 41571220988 failed `release-notes: missing non-empty '## Compatibility' section`, so no GitHub Release was published. #854 / 752b55bb added `## 兼容性` on main only (parent `6b2a8886`), leaving the tagged tree unrepaired. rc.15 (#856 / 0e6529bf) had to republish the unpublished rc.14 content; release-branch tip `e70eb28a` and the rc.15 notes both record that the rc.14 tag still points at `6b2a8886` with no GitHub Release.
- Fix: #854 repairs the notes on main. The publishable notes are `docs/releases/v1.1.0-rc.15.md` (#856 / 0e6529bf). The rc.14 tag still points at `6b2a8886`.
- Done right: before #856 merged, comment 6597046984 recorded `node scripts/validate-release-notes.mjs v1.1.0-rc.15` → `{"ok":true,"tag":"v1.1.0-rc.15"}` at `e70eb28a`, and that commit's notes include `## 兼容性`.
- Seen 1× (2026-11-03).

**The extension version guard must bind the newest release-shaped tag; a green SKIP, or a pass against an archive or older tag, is not a version check.**
- #852 CI run 41561697747 was green while the guard printed `SKIP · no v* release tag resolves locally or on origin`. The pre-fix guard also passed a changed tree against `archive/v1.0-evidence-semantics-20260905` (`0.1.90 -> 0.1.243`) and could bind `v1.1.0-rc.9`. #855 / fb7e6137 makes the shallow fetch create the tag ref, drops non-release `v*` names, and selects the newest release tag (`v1.1.0-rc.14`, extension `0.1.243` on that tree). rc.15 notes name this as one of the toolchain fixes that explain why rc.15 exists.
- Fix: fb7e6137 (#855).
- Done right: fb7e6137 and comment 6596788851. Eight guard tests pass, including the archive-tag and rc.9-vs-rc.14 cases, and a shallow no-tags checkout resolves `v1.1.0-rc.14`.
- Seen 1× (2026-11-03).

**Do not merge while tip-acceptance CHANGES_REQUESTED citation or contradiction blockers remain unfixed.**
- #878 squash a7899460982270d0c43e44d78fc2a170946dc719 (reviewed tip 4d2caff15c7348ab91285265f40b7f66fe536c6a) landed after grok comment 6738555309 and the droid tip-AC review on that tip both returned CHANGES_REQUESTED. Owner chose「选 A 立即合并」and the squash did not fix the blockers. On origin/main a7899460 they were still present. The #595 forward-derive claim is cited at `mcp.rs:8417-8460` in `docs/releases/v1.1.0.md` (lines 171 and 208), `docs/releases/1.1-feature-list.md` (lines 152 and 255), and `docs/_wip/v1.1-ga-checklist.md` (line 21). At this tree `mcp.rs:8414-8426` is `webchat_control_grant_covers` (a consumer of an already presented provider `*` grant) and `mcp.rs:8454` is the `caller_grant_missing` return inside `browser_resource_actuation_decision`. The forward projection is `projectWebchatControlGrants` in `edge/cloudflare/src/mcp-handler.ts` (function at 1446, wildcard push at 1466). Feature-list row 5 cites `caller_grant_missing` at `mcp.rs:8088-8092`; those lines validate SessionCreate `lane_id`. `docs/releases/v1.1.0.md:194` still writes `fd7e50c6=当前 main`. When the review was written, main was already `9d84bf83` (#877); it is now `a7899460`. Present-tense leftovers contradict the new PASS in the same documents: `docs/_wip/v1.1-586-uat-runbook.md` §0.1 row 2 still says 仍未有一次 stop 真正执行 in the same cell as the 2026-11-09 PASS; `docs/_wip/v1.1-ga-checklist.md` baseline still says 没有一次真实 stop beside the later closure; `docs/releases/v1.1.0.md:102` still says 实机 MV3 中途重载尚未在本候选上执行 beside the 2026-11-09 Air reload section. This is not the frozen-note lesson (that one is about moving an edit out of a published or tagged note).
- Fix: none for the citation and contradiction blockers. #880 / c833e5e74a66 dropped the `fd7e50c6=当前 main` clause while rewriting the GA closeout status (`git log -S fd7e50c6=当前` names c833e5e7). On origin/main c94fdebda971 the rest are still present: `mcp.rs:8417-8460` in `docs/releases/v1.1.0.md` (the D1 allow bullet and the #882 known-issue bullet), `docs/releases/1.1-feature-list.md` (row 6 and the #595 bullet), and `docs/_wip/v1.1-ga-checklist.md` (未实机验证项 cell); feature-list row 5 still cites `caller_grant_missing` at `mcp.rs:8088-8092`; `docs/releases/v1.1.0.md` still says 实机 MV3 中途重载尚未在本候选上执行 beside the 2026-11-09 reload section; `docs/_wip/v1.1-586-uat-runbook.md` §0.1 row 2 still says 仍未有一次 stop 真正执行 in the same cell as the 2026-11-09 PASS; the checklist cell still says 没有一次真实 stop beside the later closure. #890, #892, and #894 edited these docs and did not retarget the citations. Not a new merge over an open CHANGES_REQUESTED: #892 fixed its own four blockers in 659c75608c2c before merge. Seen stays 1×.
- Done right: the same tip-AC comments named `projectWebchatControlGrants` (~1446-1466), `caller_grant_missing` at `mcp.rs:8454`, and asked to date the old stop/MV3 sentences before merge. A follow-up that retargets those citations and dates the leftover sentences would close it.
- Seen 1× (2026-11-09).

**Do not merge a release note that still calls a device generation current after the same commit has already recorded the newer generation.**
- #892 merge 9e932686bc6f (2026-11-10) included pre-merge fix 659c75608c2c for the four acceptance blockers, then review 5837308105 returned LGTM. The merged tree still said, in `docs/releases/1.1-feature-list.md`, that the stable `v1.1.0` build had still not been installed on PROD and that MBP was untouched at `rust-cbad5ce77ca06ce6`, and in `docs/releases/v1.1.0.md` that MacBook Pro 仍为 `rust-cbad5ce77ca06ce6` 未升级, while the same merge's post-release section already listed `qingxian-macbookpro` on `rust-71a403e1fba888c2`. Same-day #894 / 221219df1958 rewrote those sentences in the two release docs. The Worker and Store lines in #894 are later facts (#893 / 06576944 merged after #892; the Store upload was not in the #892 tree) and are not this lesson. `docs/_wip/v1.1-ga-checklist.md` at c94fdebda971 still says MacBook Pro 仍为 `rust-cbad5ce77ca06ce6` 未升级 inside the 未实机验证项 cell. This is not the frozen-note lesson and not the validate-release-notes lesson: annotated tag `v1.1.0` (`8a200eb012d7`, peels to 70eb5fb7) and `origin/release/v1.1.0-prep` `d8196247558d` share tree `5c2dcfde79c3`, and `validate-release-notes` on that note is `ok:true`. A later main edit does not change that tagged tree.
- Fix: partial. 221219df1958 (#894) dates the stale generation sentences in `docs/releases/v1.1.0.md` and `docs/releases/1.1-feature-list.md`. The checklist cell is still stale.
- Done right: 659c75608c2c fixed the four sites the #892 acceptance named before merge. The sites #894 still had to rewrite were outside that list.
- Seen 1× (2026-11-10).
