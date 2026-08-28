# Release model — Runtime, Extension, and Contract Compatibility

Status: **stable `v0.4.0` published** (2026-08-28). This file is the contributor-facing definition of how releases are sliced. GA gate status lives in [`ga-release-gate.md`](./ga-release-gate.md).

## Three release planes (independent lifecycles)

| Plane | What ships | Version source | Consumer update path | Bound to runtime tag? |
| --- | --- | --- | --- | --- |
| **Runtime Release** | `herdr-mcp` Rust binary, `release-manifest.json`, platform SHA256 sidecars | `crates/herdr-mcp/Cargo.toml` → Git tag `v*` | `herdr-mcp update check/apply` on `stable` or `preview` channel | **Yes** — manifest drives updater |
| **Browser Extension Release** | `herdr-mcp-extension-<version>.zip` on the same GitHub Release page | `extension/manifest.json` `version` | Manual load / future store; `native-host install` binds to active runtime | **No** — convenience bundle only |
| **Contract Compatibility** | MCP epoch, tool catalog, state schema, Edge OAuth/MCP surface | Code + release manifest fields | Edge deploy + Link + runtime generation together | **Coupled by epoch**, not by zip filename |

**Critical rule:** the extension zip attached to a Runtime Release is a **distribution artifact**, not a lifecycle binding. Shipping extension `0.1.68` beside runtime `0.4.0` does **not** mean the updater upgrades the extension or that extension version must equal runtime version.

An extension-only change must **not** force a Rust runtime version bump. If an extension version needs distribution between runtime releases, use or add an extension-only release path (for example `extension-v*` / `extension-release.yml`) rather than creating `0.4.x` only to carry a zip.

## Runtime Release (authoritative product version)

- **Workflow:** `.github/workflows/rust-release.yml` on tag push `v*`.
- **Verify gate:** Rust fmt/clippy/test, `npm` build/test/edge, extension smoke, site build, `git diff --check`.
- **Build:** cross-target binaries per `.github/rust-release-targets.json`.
- **Manifest:** `scripts/build-rust-release-manifest.mjs` — lists **platform binaries only** (extension zip deliberately excluded from manifest asset compare; see workflow `Verify immutable release identity`).
- **Publish:** GitHub Release; prerelease when tag contains `-` (e.g. `v0.4.0-alpha.19`, `v0.4.0-rc.1`); stable when tag is plain `v0.4.0`.
- **Attestation:** `actions/attest` on release bundle.
- **Recovery:** `.github/workflows/rust-release-recover.yml` for attested recovery publishes (fail-closed on identity mismatch).

**User-facing version** = Runtime Release version (`herdr-mcp --version`, README, install docs). `package.json` remains site/extension build tooling — **not** the runtime product version (G1).

### Update channels

| Channel | Discovers | Typical use |
| --- | --- | --- |
| `stable` | Tags with empty semver prerelease (`0.4.0`) | Default after GA |
| `preview` | Prerelease tags (`0.4.0-rc.1`, `0.4.0-alpha.19`) | Maintainer rehearsal |

Installed generations are content-addressed (`rust-<sha256-prefix>`). Update applies a new generation and switches `runtime/current`; rollback reactivates a prior generation without rebuild.

## Browser Extension Release

- **Pack step:** `node scripts/pack-extension.mjs` inside `rust-release.yml` `manifest` job.
- **Artifact name:** `herdr-mcp-extension-<extension-version>.zip` (+ sha256 sidecar).
- **On GitHub Release:** uploaded alongside binaries for one-click download.
- **Not in updater manifest:** extension is not selected by `update apply`; operators install/load separately and run `native-host install` against the **active runtime**.

### Extension vs runtime version matrix (example: `v0.4.0` stable)

| Artifact | Version | Notes |
| --- | --- | --- |
| Runtime binary | `0.4.0` | Updater + service |
| Extension zip on Release | `0.1.68` | Bundled convenience; independent semver |
| Native Messaging host | Managed by runtime generation | `native-host status` must show `runtime_matches_current=true` |

## Contract Compatibility (shared public surface)

These must stay aligned across Edge, Link, and runtime for a given **contract epoch**:

- MCP tool catalog (GA: epoch **2**, **18 tools**)
- `state_schema` in release manifest (currently **4**)
- OAuth issuer / MCP endpoint behavior on public Edge
- Link `production_ready` seal and health `runtime=rust` when production Link is Rust

Edge Worker deploy is a **separate operator action** (`wrangler` / install docs §6). A Runtime Release does not auto-deploy Edge; conversely Edge deploy does not change the installed runtime generation.

## Ownership boundaries (see also `AGENTS.md`)

| Identity | Location | Never confuse with |
| --- | --- | --- |
| Source checkout | git worktree | active runtime |
| Build artifact | `target/*/herdr-mcp` | installed generation |
| Installed generation | `~/.config/herdr-mcp/runtime/generations/rust-*` | repo `target/` |
| Active runtime | `~/.config/herdr-mcp/runtime/current/herdr-mcp` | git `HEAD` |
| User CLI | `~/.local/bin/herdr-mcp` → `runtime/current` | repo `bin/herdr-mcp` |

## Alpha release retention policy

Historical prerelease tags **`v0.4.0-alpha.17` through `v0.4.0-alpha.19`** and **`v0.4.0-rc.1`** remain on GitHub for:

- preview-channel update rehearsal evidence (G9/G10)
- rollback baselines recorded in service state
- audit trail for GA closure

They are **superseded** by `v0.4.0` stable for new installs. Do **not** delete GitHub Releases/tags as part of post-GA cleanup.

## Post-GA workflow recommendations (docs only — not implemented here)

| Item | Recommendation |
| --- | --- |
| Dedicated `extension-release.yml` | Optional later workflow: tag `extension-v*` → pack zip → GitHub Release or store pipeline **without** runtime bump |
| Extension version in manifest metadata | Optional non-updater field for docs automation only |
| Store / CRX pipeline | See `docs/_wip/browser-extension-development-and-store-release.md` |
| Windows runtime target | Already published as `x86_64-pc-windows-msvc`; keep support claims conservative until G19 Windows end-to-end UAT seals |

## Related

- [`AGENTS.md`](../AGENTS.md) — binary/runtime hard rules
- [`ga-release-gate.md`](./ga-release-gate.md) — GA SSOT
- [`ga-candidate-status.md`](./ga-candidate-status.md) — live stable snapshot
- [`exit-alpha-checklist.md`](./exit-alpha-checklist.md) — G1 unification runbook
- [`i18n/en/install.md`](./i18n/en/install.md) — user install path
