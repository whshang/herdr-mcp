# Exit-alpha checklist (G1) — `0.4.0` stable version unification

Docs-only planning runbook. **Do not declare full GA until remaining vetoes are cleared.**

SSOT for gate status: [`docs/ga-release-gate.md`](./ga-release-gate.md). **Live stable snapshot:** [`docs/ga-candidate-status.md`](./ga-candidate-status.md) (`v0.4.0` stable published; dogfood baseline `0.4.0-alpha.19` post G9/G10 rollback).

**FREEZE:** `0.4.0-alpha.19` = final alpha candidate. **No alpha.20.** No delete release/branch/worktree.

## Preconditions (all required before G1 / GA declare)

| Gate | Status (2026-08-28) | Notes |
| --- | --- | --- |
| G18 | **PASS** | Second Mac clean install + public MCP (alpha.17 baseline) |
| G6 / G7 | **PASS** | Dogfood public matrix sealed 2026-08-28 (alpha.19) |
| G12 | **PASS** | Dogfood long-exec via public path |
| G2 | **PASS** | `v0.4.0` stable tag-path publish — run `33157370273` |
| G9 / G10 | **PASS** | Preview `alpha.19↔rc.1` + **stable `alpha.19↔0.4.0`** PASS |
| G20–G22 | **PASS** | User install paths reference `v0.4.0` stable (docs freeze) |
| G15 | **PARTIAL** | Second Mac extension sealed; dogfood uat singleton |
| G17 | **PARTIAL** | Public path OK; full security matrix open |
| G4 | **PARTIAL** | Second Mac stable clean install from `v0.4.0` Release not yet sealed |
| G25 | **PARTIAL** | G4 UAT open; dogfood still `alpha.19` after rehearsal |

## rc.1 path (executed 2026-08-28)

```text
1. Tag v0.4.0-rc.1 (#138 Cargo bump + #139 Cargo.lock) — DONE
2. Rust Release run 33155520284 — publish PASS
3. Preview: update check → apply → rc.1 — PASS
4. native-host / doctor post-apply — PASS
5. rollback → alpha.19 — PASS
```

Evidence: `docs/_wip/g910-rc1-stable-rehearsal-20260828.json` (gitignored).

## v0.4.0 stable path (executed 2026-08-28)

```text
1. Tag v0.4.0 (#142 Cargo bump) — DONE
2. Rust Release run 33157370273 — publish PASS
3. Stable: update check → apply → 0.4.0 — PASS
4. native-host / doctor / link post-apply — PASS
5. rollback → alpha.19 — PASS
6. G20–G22 docs freeze — DONE (user paths reference v0.4.0 stable)
```

Evidence: `docs/_wip/g910-stable-v040-20260828.json` (gitignored).

## Version surfaces today vs stable target

| Surface | Live today | Stable target | Status |
| --- | --- | --- | --- |
| Rust runtime (dogfood) | `0.4.0-alpha.19` | `0.4.0` | Post-rehearsal rollback; optional apply |
| Git tag / GitHub Release | `v0.4.0` | `v0.4.0` | **DONE** |
| User docs / README | `v0.4.0` stable primary | `0.4.0` | **DONE** (G20–G22) |
| `crates/herdr-mcp/Cargo.toml` (main) | `0.4.0` | `0.4.0` | **DONE** |
| Update channel (dogfood) | `stable` | `stable` | **DONE** |
| `package.json` | `0.3.32` | **not** runtime version | N/A |

## G1 unification assessment (2026-08-28 post docs freeze)

| Surface | Unified to `0.4.0`? | Gap |
| --- | --- | --- |
| Cargo.toml / binary / Git tag / Release | **Yes** | — |
| User docs / README | **Yes** | G20–G22 PASS |
| Dogfood runtime | **Partial** | Rolled back to `alpha.19` after stable G9/G10 rehearsal |
| G4 second Mac UAT | **Open** | Prior G18 used `alpha.17`; re-seal from `v0.4.0` stable |

## Maintainer sequence (optional dogfood stable apply — after G4 PASS)

```bash
# 1. Preflight (read-only, independent shell)
herdr-mcp --version
herdr-mcp service status
herdr-mcp link seal status

# 2. Stable apply (independent shell; dogfood default instance)
herdr-mcp update check    # stable channel → 0.4.0
herdr-mcp update apply
herdr-mcp doctor
herdr-mcp --version       # expect 0.4.0

# 3. Second Mac clean install regression from v0.4.0 Release (G4)
# See docs/i18n/en/clean-machine-uat.md §One-command bootstrap
```

## Evidence to flip G1 → PASS

- `herdr-mcp --version` → `0.4.0` on dogfood (optional stable apply)
- G4 second Mac clean install from [`v0.4.0` Release](https://github.com/whshang/herdr-mcp/releases/tag/v0.4.0)
- `package.json` explicitly **not** the runtime product version

## Explicit non-steps

- **Do not** tag `v0.4.0-alpha.20` (alpha.19 is final candidate).
- **Do not** rename `package.json` to `0.4.0` as a G1 shortcut.
- **Do not** delete/move historical tags (`v0.4.0-alpha.*`, `v0.4.0-rc.1`).

## Related

- [`docs/ga-candidate-status.md`](./ga-candidate-status.md)
- [`docs/ga-release-gate.md`](./ga-release-gate.md)
- [`docs/i18n/en/clean-machine-uat.md`](./i18n/en/clean-machine-uat.md)
- [`docs/i18n/zh-CN/clean-machine-uat.md`](./i18n/zh-CN/clean-machine-uat.md)
