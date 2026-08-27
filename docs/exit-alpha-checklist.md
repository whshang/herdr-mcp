# Exit-alpha checklist (G1) — `0.4.0` stable version unification

Docs-only planning runbook. **Do not cut `v0.4.0` (non-prerelease) until owner UAT closes the remaining GA vetoes.**

SSOT for gate status: [`docs/ga-release-gate.md`](./ga-release-gate.md). Owner execution paths: **Owner ChatGPT UAT pack** and [`docs/i18n/en/clean-machine-uat.md`](./i18n/en/clean-machine-uat.md) in that file.

## Preconditions (all required before G1 work)

Do **not** start version unification until these rows are honest **PASS** (not PARTIAL):

| Gate | What must be sealed first |
| --- | --- |
| G18 | Second Mac **default instance** clean install: binary → install → doctor/status/update-check → optional update/rollback |
| G6 / G7 | Public ChatGPT OAuth → fresh `tools/list` → epoch 2 / 18 tools → read-only + bounded mutation + long-exec smoke |
| G15 | Chrome Load unpacked + `native-host install` + binding smoke on a machine that owns the Chrome profile (second Mac or owner maintenance window on dogfood default instance) |
| G17 | Clean-machine + public security acceptance recorded |
| G9 / G10 | Stable-channel `update apply` and controlled `rollback` on the clean default instance |
| G25 | No remaining GA veto (see scorecard) |

Same-machine `--instance uat` evidence (alpha.16) **does not** satisfy G18/G15/G6/G7 public segments.

## Version surfaces today vs stable target

| Surface | Live / repo today | Stable target | Notes |
| --- | --- | --- | --- |
| Rust runtime (`herdr-mcp --version`) | `0.4.0-alpha.16` | `0.4.0` | Authoritative product version |
| `crates/herdr-mcp/Cargo.toml` | `0.4.0-alpha.16` | `0.4.0` | Bump before tag |
| Git tag / GitHub Release | `v0.4.0-alpha.16` | `v0.4.0` | Triggers `.github/workflows/rust-release.yml` |
| User CLI symlink | `~/.local/bin/herdr-mcp` → `runtime/current` | unchanged pattern | After release: `update apply` on dogfood |
| `package.json` `version` | `0.3.32` | **unchanged or tooling-only bump** | Must **not** become the runtime product version in README/docs (G1) |
| Extension `manifest.json` | `0.1.64` (independent semver) | release zip from tag | Extension artifact version ≠ runtime semver; document mapping in release notes |
| Docs / scorecard | alpha.16 references | `0.4.0` stable wording | Flip G1 row only after live dogfood runs stable |

## Maintainer sequence (after preconditions)

Run from a repo checkout; Rust gate is authoritative for runtime changes.

```bash
# 1. Preflight live (read-only)
herdr-mcp --version
herdr-mcp service status
herdr-mcp link seal status

# 2. Bump runtime version (example — edit Cargo.toml to 0.4.0, no -alpha)
cargo fmt --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace

# 3. Refresh GA docs on the same commit
#    - docs/ga-release-gate.md: date, G1/G25 PASS, remove "do not tag stable" once true
#    - docs/i18n/en|zh-CN/clean-machine-uat.md: TAG=v0.4.0 examples
#    - README*: stable install path points at v0.4.0 Release

# 4. Tag only when scorecard vetoes are cleared
git tag -a v0.4.0 -m "herdr-mcp 0.4.0 stable"
git push origin v0.4.0
# CI rust-release.yml publishes binaries + extension zip + manifest

# 5. Post-release verification (independent shell)
gh release view v0.4.0 -R whshang/herdr-mcp
herdr-mcp update check          # expect stable channel finds v0.4.0
herdr-mcp update apply          # dogfood cutover — NOT from herdr_exec session
herdr-mcp doctor
herdr-mcp --version             # expect 0.4.0, no alpha

# 6. Re-run clean-machine-uat on second Mac from v0.4.0 Release (confirm no regression)
```

## Evidence to paste into scorecard (flip G1 → PASS)

Non-secret bullets only:

- `herdr-mcp --version` → `0.4.0` on dogfood after managed `update apply`
- GitHub Release `v0.4.0` asset list + manifest SHA verification command output
- Second Mac default-instance clean install PASS summary (from clean-machine-uat §A–C)
- Public ChatGPT UAT PASS summary (Owner pack steps 1–7)
- Explicit note that `package.json` version is **not** the runtime product version

## Explicit non-steps

- **Do not** tag `v0.4.0` while G18/G6/G7/G15 owner paths remain PARTIAL.
- **Do not** rename `package.json` to `0.4.0` as a shortcut for G1.
- **Do not** uninstall the dogfood `--instance uat` service unless following documented cleanup after owner UAT; uat does not block stable tagging.
- **Do not** perform ChatGPT OAuth from an unattended agent session.

## Related

- [`docs/ga-release-gate.md`](./ga-release-gate.md) — G1–G25 scorecard and Owner ChatGPT UAT pack
- [`docs/i18n/en/clean-machine-uat.md`](./i18n/en/clean-machine-uat.md) — Path A second Mac canonical G18
- [`docs/i18n/zh-CN/clean-machine-uat.md`](./i18n/zh-CN/clean-machine-uat.md) — 中文 runbook
