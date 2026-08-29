# Exit-alpha checklist (G1) — `0.4.0` stable version unification

Docs-only planning runbook. **Do not declare full GA until remaining vetoes are cleared.**

SSOT for gate status: [`docs/ga-release-gate.md`](../../ga-release-gate.md). **Live stable snapshot:** [`docs/ga-candidate-status.md`](../../ga-candidate-status.md) (`v0.4.0` stable published; dogfood `0.4.0`). **Release model:** [`docs/release-model.md`](../../release-model.md).

**Historical alpha freeze (completed):** `0.4.0-alpha.19` was the final alpha candidate and there was no alpha.20. Immutable alpha tags/Releases remain retained per the release-model policy; merged/obsolete development branches and worktrees may be cleaned after live safety checks.

## Preconditions (2026-08-28 post-G4)

| Gate | Status | Notes |
| --- | --- | --- |
| G4 | **PASS** | Second Mac clean install from `v0.4.0` stable Release (pi-ga-20260828) |
| G18 | **PASS** | Second Mac public MCP (historical alpha.17 + stable G4) |
| G6 / G7 | **PASS** | Dogfood public matrix sealed 2026-08-28 |
| G12 | **PASS** | Dogfood long-exec via public path |
| G2 | **PASS** | `v0.4.0` stable tag-path publish — run `33157370273` |
| G9 / G10 | **PASS** | Preview `alpha.19↔rc.1` + stable `alpha.19↔0.4.0` |
| G20–G22 | **PASS** | User install paths reference `v0.4.0` stable |
| G1 | **PASS** | Dogfood runtime `0.4.0`; unified stable surfaces |
| G15 | **PARTIAL** | Second Mac extension sealed; dogfood uat singleton |
| G17 | **PARTIAL** | Public path OK; full security matrix open |
| G25 | **PARTIAL** | G14/G15 veto-adjacent; **do not declare GA** |

## rc.1 path (executed 2026-08-28)

```text
1. Tag v0.4.0-rc.1 (#138 Cargo bump + #139 Cargo.lock) — DONE
2. Rust Release run 33155520284 — publish PASS
3. Preview: update check → apply → rc.1 — PASS
4. native-host / doctor post-apply — PASS
5. rollback → alpha.19 — PASS
```

Evidence: [`g910-rc1-stable-rehearsal-20260828.json`](./g910-rc1-stable-rehearsal-20260828.json) (archived, tracked).

## v0.4.0 stable path (executed 2026-08-28)

```text
1. Tag v0.4.0 (#142 Cargo bump) — DONE
2. Rust Release run 33157370273 — publish PASS
3. Stable: update check → apply → 0.4.0 — PASS
4. native-host / doctor / link post-apply — PASS
5. rollback → alpha.19 — PASS
6. G20–G22 docs freeze — DONE
7. G4 second Mac stable clean install — PASS (pi-ga-20260828)
```

Evidence: the one-off stable-channel JSON was never tracked and is no longer retained; use [`ga-release-gate.md`](../../ga-release-gate.md) + Rust Release run [`33157370273`](https://github.com/whshang/herdr-mcp/actions/runs/33157370273), plus [`g4-second-mac-stable-v040-uat-20260828.md`](./g4-second-mac-stable-v040-uat-20260828.md).

## Version surfaces (2026-08-28)

| Surface | Value | Notes |
| --- | --- | --- |
| Rust runtime (dogfood) | `0.4.0` | generation `rust-621d74d268b5299a` |
| Git tag / GitHub Release | `v0.4.0` | stable, not prerelease |
| User docs / README | `v0.4.0` stable | G20–G22 PASS |
| `crates/herdr-mcp/Cargo.toml` | `0.4.0` | product version |
| Update channel (dogfood) | `stable` | config.toml |
| `package.json` | `0.3.32` | **not** runtime version |
| Extension zip on Release | `0.1.68` | independent semver (see release-model) |

## G1 unification assessment

| Surface | Unified to `0.4.0`? | Gap |
| --- | --- | --- |
| Cargo.toml / binary / Git tag / Release | **Yes** | — |
| User docs / README | **Yes** | G20–G22 PASS |
| Dogfood runtime | **Yes** | stable apply job succeeded |
| G4 second Mac UAT | **Yes** | stable Release clean install PASS |

## Alpha release retention (do not delete)

| Tag | Role |
| --- | --- |
| `v0.4.0-alpha.17`–`alpha.19` | Preview-channel rehearsal + rollback baselines |
| `v0.4.0-rc.1` | rc rehearsal evidence |
| `v0.4.0` | **Current stable** for new installs |

Superseded for **new installs** only — tags remain on GitHub for audit.

## Explicit non-steps

- **Do not** tag `v0.4.0-alpha.20`.
- **Do not** rename `package.json` to `0.4.0` as a G1 shortcut.
- **Do not** delete historical tags/releases.

## Related

- [`docs/ga-candidate-status.md`](../../ga-candidate-status.md)
- [`docs/ga-release-gate.md`](../../ga-release-gate.md)
- [`docs/release-model.md`](../../release-model.md)
- [`docs/history/ga/README.md`](./README.md)
- [`docs/i18n/en/clean-machine-uat.md`](../../i18n/en/clean-machine-uat.md)
