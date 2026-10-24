# Rules

Hard rules and code conventions promoted from FINDINGS.md after the same lesson
was recorded on 3 distinct review dates.

Entries use the template defined in [FINDINGS.md](FINDINGS.md).

---

**The acceptance trio (rollback, real-usage, NOT run) must appear in the landing merge commit body and in an acceptance PR comment—not only in a feature commit or the PR description.**
- After #609 landed the AGENTS.md rule, GitHub merge commits kept landing with title-only bodies. 2026-10-13: #609 (619e254a), #625 (caf1584f), #627 (d193380a); #625/#627 had empty PR comment threads. 2026-10-23 (recorded on unmerged #696, folded in here): #686 (2cec2c49), #689 (9206f2a9), #693 (bc27cd9b), #694 (bfd9035c), #692 (c7a0729b). 2026-10-24: #695 (6ba0a577) and #691 (c3a75199). On #691 and #695 the PR comment threads do carry the trio (including #695 comment 6367240440 and multiple #691 tip-accept comments); the landing merge bodies do not. Feature/PR text does not transfer to the squash-or-merge commit that lands on the target branch.
- Fix: none yet (docs process). Promoted by daily retro 2026-10-24.
- Done right: #591 (91bf2aae), #611 (05d83989), and #620 (07c91731) merge bodies each state rollback, real-usage, and NOT run; #592 also left acceptance evidence in PR comments.
- Seen 3× (2026-10-13, 2026-10-23, 2026-10-24).
