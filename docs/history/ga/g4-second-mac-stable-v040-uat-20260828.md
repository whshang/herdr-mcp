# Completed / Historical — G4 second Mac stable clean install UAT

**Gate:** G4 — install lifecycle on clean machine from **`v0.4.0` stable Release** (not alpha.17/alpha.19).  
**Session:** `pi-ga-20260828`  
**Verdict:** **PASS**

## Machine

| Field | Value |
| --- | --- |
| Host | Second Mac (pi) — default instance |
| Platform | macOS Apple Silicon |
| Prior G18 baseline | `v0.4.0-alpha.17` (superseded for G4 by this run) |

## Release identity

| Check | Result |
| --- | --- |
| GitHub Release tag | `v0.4.0` stable |
| Runtime `--version` | `0.4.0` |
| Extension zip on Release | `0.1.68` (bundled artifact; independent semver — see [`release-model.md`](../release-model.md)) |
| Update channel | `stable` at head (`available=false` after install) |

## Independent stack (no dogfood coupling)

| Layer | Result | Notes |
| --- | --- | --- |
| Cloudflare Worker | PASS | `herdr-edge-yitaidiannao-local-ga` — workers.dev UAT worker (retained) |
| Workstation Link | PASS | Via operator proxy (China / workers.dev reachability) |
| Local runtime service | PASS | `herdr-mcp install` from Release binary only |
| Native Messaging | PASS | **4** browser manifests installed |
| `doctor` | PASS | **7/7** layers green on pi scope |
| Custom domain / Tunnel | PASS (absent) | workers.dev only — matches UAT contract |

## Distinction from G18 (alpha.17)

| Gate | Evidence | Runtime source |
| --- | --- | --- |
| G18 (2026-08-28) | Second Mac public MCP loop | `v0.4.0-alpha.17` Release |
| **G4 (this run)** | Same class of clean install + doctor + extension + Link + Edge | **`v0.4.0` stable Release** |

G4 seal does not delete G18 history; it closes the stable-install veto that G18 could not satisfy with alpha binaries.

## Owner follow-up (out of agent scope)

ChatGPT Connector OAuth on the pi Worker MCP URL — owner-only per internal UAT protocol. Dogfood public G6/G7 matrix remains sealed separately on production Edge.

## Related

- [`ga-release-gate.md`](../ga-release-gate.md) — G4 scorecard row
- [`second-mac-ga-uat-agent-prompt-en.md`](./second-mac-ga-uat-agent-prompt-en.md) — protocol used for this run
- [`release-model.md`](../release-model.md) — runtime vs extension release planes
