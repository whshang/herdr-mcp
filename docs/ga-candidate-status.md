# GA Candidate Status — `0.4.0-alpha.19` (final GA candidate)

Status: **alpha candidate frozen** (2026-08-28). **Do not cut `v0.4.0` stable or `v0.4.0-rc.1`** until remaining vetoes in [`ga-release-gate.md`](./ga-release-gate.md) are honest PASS.

SSOT for gate rows: [`docs/ga-release-gate.md`](./ga-release-gate.md). Exit-alpha runbook: [`docs/exit-alpha-checklist.md`](./exit-alpha-checklist.md).

**FREEZE:** alpha.19 = final GA candidate. **No alpha.20.** No delete release/branch/worktree.

## Current candidate

| Field | Value |
| --- | --- |
| Runtime version | `0.4.0-alpha.19` |
| Git tag | `v0.4.0-alpha.19` |
| Tag commit | `4690c13d9e7304852f9f762cb783d1326f7a5e12` (#134) |
| Generation (dogfood) | `rust-3d2f685c636c3f3e` |
| Contract | epoch 2 / 18 tools / state schema 4 |
| Release URL | <https://github.com/whshang/herdr-mcp/releases/tag/v0.4.0-alpha.19> |
| Tag-path workflow | [Rust Release run 33152207094](https://github.com/whshang/herdr-mcp/actions/runs/33152207094) — verify → build → manifest → attest → **publish** all PASS |
| Connector URL (workers.dev) | `https://herdr-edge-prod.whshang.workers.dev/mcp` |
| Connector URL (custom domain) | `https://herdr-mcp.agentforme.cc.cd/mcp` |
| OAuth issuer | `https://herdr-mcp.agentforme.cc.cd` |
| Update channel (dogfood) | `preview` |

## Release identity (G2 seal)

| Check | Result | Evidence |
| --- | --- | --- |
| Tag SHA == manifest `source_commit` | PASS | `4690c13…` in `release-manifest.json` |
| Manifest `source_ref` | PASS | `refs/tags/v0.4.0-alpha.19` |
| Binary SHA (darwin aarch64) | PASS | `3d2f685c636c3f3e2c4720c9a63c703a818307af0507a68ac953268f8a009a60` |
| Updater provenance | PASS | dogfood `update check` `provenance_verified=true` |
| Attestation | PASS | `actions/attest` job green on run 33152207094 |
| Extension zip excluded from manifest assets | PASS | manifest lists 2 platform binaries only |
| Fail-closed duplicate publish | PASS | alpha.18 tag run failed identity verify; publish refuses clobber |

## G6/G7 dogfood public UAT (2026-08-28 · PASS)

Evidence (local, gitignored): `docs/_wip/g67-dogfood-public-uat-20260828.json` · runner `docs/_wip/g67-dogfood-public-uat-20260828.mjs`

| Step | Result | Notes |
| --- | --- | --- |
| Edge `/health` | PASS | HTTP 200, `contract_epoch=2` |
| OAuth (DCR+PKCE) | PASS | Same issuer/endpoints as ChatGPT Connector; refresh issued |
| `initialize` + `tools/list` | PASS | 18 tools, epoch-2 catalog |
| Read-only (`inspect`, `fs_list`, `git`) | PASS | Public `/mcp` → Edge → Link → runtime |
| Bounded mutation (`fs_write`) | PASS | Single write; duplicate blocked (`overwrite_confirmation_required`) |
| Long exec (`exec_start` → `exec_read`) | PASS | `es_107ef-1a04768766c-10` completed in 2 polls |

**Caveat:** OAuth via programmatic DCR+PKCE (ChatGPT-equivalent). ChatGPT browser UI not re-run this session; second-Mac G18 sealed Connector OAuth (alpha.17).

## G9/G10 rc.1-equivalent rehearsal (2026-08-28 · PASS)

Evidence (local, gitignored): `docs/_wip/g910-rc1-rehearsal-20260828.json`

| Step | Result |
| --- | --- |
| Preflight `alpha.19` | PASS |
| `rollback` → `alpha.18` | PASS (`rb-1787903377082-rust-3d2f685c`, guardian settled, native-host synced) |
| `doctor` / Link after rollback | PASS |
| `update check` (preview) | PASS (`v0.4.0-alpha.19` available, provenance verified) |
| `update apply` | PASS (job `upd-1787904465310-80994-3d2f685c`) |
| Post-update `doctor` / native-host / Link | PASS on `alpha.19` |

**Equivalence limits:** preview-channel `alpha.18↔alpha.19` ≠ stable-channel G9/G10 PASS.

## rc.1 path (design — not executed)

```text
v0.4.0-rc.1 tag (Cargo bump; no alpha.20)
  → GitHub Release (rust-release.yml)
  → install or update apply (preview)
  → doctor → native-host status → Link health
  → rollback → doctor → verify recovery
  → stable-channel update/rollback rehearsal
  → v0.4.0 stable tag only after stable-channel G9/G10 PASS
```

## Remaining GA blockers (honest)

| Gate | Why still open |
| --- | --- |
| G1 | Still alpha semver; no `v0.4.0` stable |
| G9 / G10 | No stable-channel N→N+1 rehearsal (blocked until rc.1/stable tag) |
| G20–G22 | Stable docs freeze (user paths still mention alpha) |
| G24 / G25 | Vetoes above; **do not tag stable** |

## Can enter `v0.4.0` formal release?

**No.** Missing: G1 exit-alpha, stable-channel G9/G10, G20–G22 docs freeze, G25 vetoes cleared.

## Next owner actions

1. Approve `v0.4.0-rc.1` tag workflow PR (version bump + docs only).
2. Run stable-channel rehearsal after rc.1 Release publishes.
3. Re-run second-Mac clean install from rc.1 Release.
4. Only then evaluate `v0.4.0` stable tag.

## Related

- [`docs/ga-release-gate.md`](./ga-release-gate.md)
- [`docs/exit-alpha-checklist.md`](./exit-alpha-checklist.md)
- [`docs/i18n/en/clean-machine-uat.md`](./i18n/en/clean-machine-uat.md)
