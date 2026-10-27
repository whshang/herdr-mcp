# Rules

Hard rules and code conventions promoted from FINDINGS.md after the same lesson
was recorded on 3 distinct review dates.

Entries use the template defined in [FINDINGS.md](FINDINGS.md).

---

**The acceptance trio (rollback, real-usage, NOT run) must appear in the landing merge commit body and in an acceptance PR comment—not only in a feature commit or the PR description.**
- After #609 landed the AGENTS.md rule, GitHub merge commits kept landing with title-only or CI-green bodies. 2026-10-13: #609 (619e254a), #625 (caf1584f), #627 (d193380a); #625/#627 had empty PR comment threads. 2026-10-23 (recorded on unmerged #696, folded from draft #697): #686 (2cec2c49), #689 (9206f2a9), #693 (bc27cd9b), #694 (bfd9035c), #692 (c7a0729b). 2026-10-24 (also on unmerged #697): #695 (6ba0a577) and #691 (c3a75199); those PR threads do carry the trio (including #695 comment 6367240440) and the landing bodies do not. 2026-10-27: title-only landings #700 (5acd4ded), #702 (f2e11165), #703 (e8350a2a), #704 (42b6aee7), #705 (f4def02e), #706 (90c13097), #708 (b0ea665f), #709 (05989687), #714 (1b8d4bdb). CI-green or quota bodies without the trio: #707 (9e5f962c), #710 (d083e900), #711 (cba036ca), #712 (80b3cc24), #715 (74feb50f). Several of those threads do carry the trio and the landing body still omits it: #702 comments 6434225214 and 6434229737, #704 review 5663193320, #706 review 5663192860, #707 comment 6437860370, #710 comment 6437891032, #711 comment 6437861119, #712 comment 6437891909, #715 comment 6437892813. #700, #703, #705, #708, #709, and #714 have no comment that states all three. Feature commits and PR descriptions do not transfer onto the commit that lands on the target branch.
- Fix: none yet (docs process). Promoted by daily retro 2026-10-27, folding unmerged #697.
- Done right: #591 (91bf2aae), #611 (05d83989), and #620 (07c91731) merge bodies each state rollback, real-usage, and NOT run. On 2026-10-27, #701 squash a99e8730 states Rollback tests, Real-usage verification, and Items not run, and PR comments 6433562579 and 6433755847 state the same three.
- Seen 4× (2026-10-13, 2026-10-23, 2026-10-24, 2026-10-27).
