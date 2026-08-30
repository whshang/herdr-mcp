---
name: engineering-robustness
description: Reference for designing, changing, debugging, testing, and releasing AI-generated code so failures become durable regression assets and completion is proven across the real delivery boundary.
---

# Engineering Robustness Reference

Reference-only module; owns no public tool and grants no mutation authority.

Use this for non-trivial implementation, bug fixes, reliability work, refactors, compatibility changes, background/state-machine work, and releases. Optimize for code that remains understandable, testable, and safe to evolve after many AI-generated iterations.

## 1. Preserve architecture before increasing code volume

Before changing a subsystem, identify its current layers, ownership boundaries, invariants, state transitions, and extension seams. Read the relevant project rules and architecture docs first. Keep architecture documentation current when a durable boundary or rationale changes.

Prefer one coherent abstraction over repeated special cases, but do not create abstractions without a demonstrated seam. A fast implementation that creates another state owner, background listener, permission, setting, compatibility branch, or lifecycle is a long-term maintenance cost.

Apply the minimum-entity rule:

- avoid a new setting when a safe default is sufficient;
- avoid a new privileged permission when an existing boundary can express the requirement;
- avoid a resident watcher/background loop unless the product needs continuous observation;
- avoid compatibility branches without a supported user/version contract;
- avoid broadening cleanup/update scope merely because another theoretical case exists.

## 2. Test the ways a result can look correct while being wrong

Happy paths are necessary but rarely the highest-value reliability tests. Prioritize **silent wrongness**: states where the operation reports success, returns plausible data, or produces a green test while the product is already incorrect.

Always consider failure classes such as:

- data changes after a scan/read but before the result is applied;
- process/status inspection fails, returns partial data, or observes a stale generation;
- a command exits successfully but the intended application/runtime/artifact did not change;
- an old task, event, stream, or request returns late and overwrites newer state;
- timeout/disconnect leaves delivery or mutation outcome uncertain;
- cache or persisted state belongs to an older generation/session/identity;
- local source and tests are correct while a packaged, signed, uploaded, CDN, update-feed, or user-visible artifact is stale;
- retry/reconnect duplicates a side effect;
- concurrent work races on shared files, state, active target, or runtime ownership;
- a UI or API displays a plausible stale state after the authoritative backend advanced.

When designing tests, spend more effort on these boundaries than on multiplying easy nominal cases.

## 3. Turn every real bug into a durable asset

A bug fix is incomplete until it leaves evidence that prevents the same failure class from quietly returning.

Use this regression loop:

1. Reproduce the failure with the smallest trustworthy evidence.
2. Add a regression test or deterministic check that fails against the old behavior when feasible.
3. Implement the smallest coherent fix.
4. Run the focused regression test.
5. Search sibling paths for the same failure class: same state transition, retry pattern, stale result path, command-success assumption, generation boundary, or cleanup rule.
6. Add sibling tests/fixes where evidence justifies them; do not speculative-refactor unrelated code.
7. If the reason is non-obvious and likely to be rediscovered, update the relevant rule/reference/architecture note with **why the tempting alternative is unsafe**.
8. Re-run the broader relevant gate.

The valuable asset is not the number of tests. It is the accumulated set of real failure modes, invariants, and reasons that future agents can reuse.

## 4. Keep tests and rules modular and fresh

Tests remember inputs and expected outcomes. Rules/references remember product boundaries and historical rationale. Use both.

Keep detailed rules close to their subsystem and load them only when the current change touches that domain. Convert repeated deterministic review steps into Skills or scripts. Do not force every task to ingest all historical rules.

When behavior is intentionally removed or redesigned, update or delete stale tests and rules in the same change. A stale safety rule can be as harmful as missing documentation because future agents will optimize around a constraint that no longer exists.

## 5. Let AI execute the verification loop

For AI-generated code, the agent that changes the code should normally execute the relevant tests and checks itself. Do not stop at “tests should be run” or hand routine verification back to a human.

Preferred loop:

```text
inspect invariants / current state
  -> implement minimal change
  -> focused regression tests
  -> sibling failure-class scan
  -> broader relevant test/build/contract gate
  -> real boundary verification
  -> Git/evidence review
  -> update durable rule/reference when justified
```

If a check fails, diagnose and fix it, then rerun the smallest affected check before the broader gate. Human intervention is reserved for genuinely external, credentialed, irreversible, visual-judgment, policy, or physical-device boundaries that the agent cannot safely exercise.

Never treat process exit code 0, a worker saying “done,” or one green unit suite as sufficient completion evidence.

## 6. Verify state planes separately

Source correctness, repository state, CI, built artifacts, deployed files, activated runtime, and user-visible behavior are different state planes. A release can be correct in one and stale in another.

For the task at hand, explicitly identify the relevant planes and verify each required transition. Typical release planes are:

```text
working tree
  -> committed Git state
  -> clean-machine CI
  -> built/package artifact
  -> signature/notarization or integrity metadata
  -> uploaded/public distribution object
  -> update feed/index/manifest
  -> installed/activated runtime
  -> user-visible behavior from the real client path
```

Do not infer a later plane from an earlier one. “Source is correct” does not prove the public file changed. “CI is green” does not prove the signed artifact matches the commit. “Upload succeeded” does not prove the client receives the new object.

Use the same client/boundary that matters to the user whenever practical.

## 7. Use CI as independent clean-machine evidence

Local verification is fast feedback; CI is an independent environment and final consistency gate. CI should rerun the important deterministic checks from a clean machine and verify cross-file/product consistency that developers can easily forget, such as generated site output, localization sets, manifests/update feeds, project files, public deployment files, schemas, or contract snapshots.

Keep one project-level verify/release gate that composes the relevant checks when practical. A green gate is meaningful only when its inputs represent the current commit and its outputs correspond to the artifacts that will actually ship.

## 8. Completion gate

Before declaring a non-trivial engineering task complete, answer these with evidence:

- Architecture: did the change preserve or intentionally update ownership and invariants?
- Regression: does a test/check cover the discovered failure, especially if it was silently wrong?
- Similarity: were sibling paths searched for the same failure class?
- Scope: did the change avoid unnecessary settings, permissions, listeners, branches, and state owners?
- Verification: did AI run the focused and broader relevant checks itself?
- Delivery: were all relevant state planes verified independently?
- Freshness: were obsolete tests/rules removed or updated?
- Evidence: can another agent see the Git/test/runtime/artifact facts needed to trust completion?

If an outcome is uncertain, stop and inspect. Never manufacture closure by retrying an uncertain mutation or by substituting a weaker green signal for the real boundary.
