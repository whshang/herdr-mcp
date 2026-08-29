# Release model — Runtime, Extension, and Contract Compatibility

Status: **current runtime stable `v0.4.1` published** (2026-08-28). First-GA `v0.4.0` evidence remains historical and immutable. This file is the contributor-facing definition of how releases are sliced. GA gate status lives in [`ga-release-gate.md`](./ga-release-gate.md).

## Three release planes (independent lifecycles)

| Plane | What ships | Version source | Consumer update path | Bound to runtime tag? |
| --- | --- | --- | --- | --- |
| **Runtime Release** | `herdr-mcp` Rust binary, `release-manifest.json`, platform SHA256 sidecars | `crates/herdr-mcp/Cargo.toml` → Git tag `v*` | `herdr-mcp update check/apply` on `stable` or `preview` channel | **Yes** — manifest drives updater |
| **Browser Extension Release** | Chrome Web Store item | `extension/manifest.json` `version` | Chrome Web Store automatic update; `native-host install` binds the Store origin to the active runtime | **No** — independent Store lifecycle |
| **Contract Compatibility** | MCP epoch, tool catalog, state schema, Edge OAuth/MCP surface | Code + release manifest fields | Edge deploy + Link + runtime generation together | **Coupled by epoch**, not by zip filename |

**Critical rule:** Runtime GitHub Releases do **not** distribute the browser extension. The extension is a Chrome Web Store product with its own version and update cadence. Browser-extension `0.1.x` and runtime `0.4.x` evolve independently; proximity in release time does not create a semver or updater coupling between them.

An extension-only change must **not** force a Rust runtime version bump. Maintainers may use `scripts/pack-extension.mjs` to build a deterministic Store-upload / explicit unpacked-UAT package, but that zip is not an end-user Runtime Release asset.

## Runtime Release (authoritative product version)

- **Workflow:** `.github/workflows/rust-release.yml` on tag push `v*`.
- **Verify gate:** Rust fmt/clippy/test, `npm` build/test/edge, extension smoke, site build, `git diff --check`.
- **Build:** cross-target binaries per `.github/rust-release-targets.json`.
- **Manifest:** `scripts/build-rust-release-manifest.mjs` — lists the complete Runtime Release binary set; the browser extension is not a Runtime Release asset.
- **Publish:** GitHub Release; prerelease when semver has a prerelease component (for example historical `v0.4.0-rc.1`); stable when the tag is plain semver (current: `v0.4.1`).
- **Attestation:** `actions/attest` on release bundle.
- **Recovery:** `.github/workflows/rust-release-recover.yml` for attested recovery publishes (fail-closed on identity mismatch).

**User-facing version** = Runtime Release version (`herdr-mcp --version`, README, install docs). `package.json` remains site/extension build tooling — **not** the runtime product version (G1).

### Update channels

| Channel | Discovers | Typical use |
| --- | --- | --- |
| `stable` | Tags with empty semver prerelease (current: `0.4.1`) | Default |
| `preview` | Prerelease tags (`0.4.0-rc.1`, `0.4.0-alpha.19`) | Maintainer rehearsal |

Installed generations are content-addressed (`rust-<sha256-prefix>`). Update applies a new generation and switches `runtime/current`; rollback reactivates a prior generation without rebuild.

## Browser Extension Release

- **Store packaging:** maintainers run `node scripts/pack-extension.mjs` only for Chrome Web Store upload or explicit unpacked UAT.
- **Runtime release boundary:** `.github/workflows/rust-release.yml` never attaches an extension zip.
- **Distribution:** end users install the extension from the Chrome Web Store. A locally packed zip is maintainer-only Store-upload / explicit unpacked-UAT input, not a Runtime GitHub Release asset.
- **Updater separation:** `update apply` updates the Rust runtime only. After a Store install, `native-host install` binds the official Store origin to the **active runtime**; explicit unpacked development remains a maintainer override.

### Extension vs runtime version matrix (current snapshot)

| Artifact | Version | Notes |
| --- | --- | --- |
| Runtime binary | `0.4.2` | Current stable; updater + service |
| Browser extension source | `0.1.76` | Current development source; Chrome Web Store published/review version may lag and must be checked independently |
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

They were **superseded** by the first stable `v0.4.0`; the current runtime stable is `v0.4.1`. Do **not** delete historical GitHub Releases/tags as part of cleanup.

## Post-GA workflow recommendations (docs only — not implemented here)

| Item | Recommendation |
| --- | --- |
| Dedicated `extension-release.yml` | Optional later automation: tag `extension-v*` → deterministic Store-upload / CI artifact → Chrome Web Store submission **without** runtime bump; do not create a second end-user GitHub extension channel |
| Extension version in manifest metadata | Optional non-updater field for docs automation only |
| Store / CRX pipeline | See `docs/_wip/browser-extension-development-and-store-release.md` |
| Windows runtime target | Already published as `x86_64-pc-windows-msvc`; keep support claims conservative until G19 Windows end-to-end UAT seals |

## Related

- [`AGENTS.md`](../AGENTS.md) — binary/runtime hard rules
- [`ga-release-gate.md`](./ga-release-gate.md) — GA SSOT
- [`ga-candidate-status.md`](./ga-candidate-status.md) — live stable snapshot
- [`history/ga/exit-alpha-checklist.md`](./history/ga/exit-alpha-checklist.md) — G1 unification runbook (archived)
- [`i18n/en/install.md`](./i18n/en/install.md) — user install path
