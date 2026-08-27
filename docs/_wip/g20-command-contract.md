# G20 / G1 WIP notes (not published site content)

Status: alpha in progress. This folder is intentionally excluded from the docs site build.

## G1 debt (do not strip yet)

Accurate prerelease / alpha labels remain in Release tags, binaries, and some docs until the G1 version policy decides a single stable product version. Do not rewrite `0.4.0-alpha.x` labels into GA/stable claims in this PR.

## G20 command contract — known FAIL vs Rust CLI (`herdr-mcp --help`)

Rust top-level surface today (authoritative for this scan):

```text
herdr-mcp version
herdr-mcp status
herdr-mcp doctor
herdr-mcp config [path|show|init]
herdr-mcp dev [--dry-run]
herdr-mcp candidate [--port 8873]
herdr-mcp service <install [--adopt-node]|status|start|stop|restart|rollback|uninstall>
herdr-mcp update <check [--manifest URL]|apply [--manifest URL]|status>
herdr-mcp native-host <install|status|uninstall|rollback>
herdr-mcp extension-host [chrome-extension://.../]
```

### Still documented outside the primary install path (FAIL until rewritten or aliased)

| Documented command | Where it still appears | Reality |
| --- | --- | --- |
| `herdr-mcp start` / `stop` / `restart` / `logs` / `logs -f` | `docs/i18n/*/cli-reference.md` | Bash compatibility wrapper (`bin/herdr-mcp`), not Rust top-level help |
| `herdr-mcp watchdog install` / `watchdog status` | `cli-reference.md`, older install remnants elsewhere | Legacy Bash watchdog; not Rust user path |
| `herdr-mcp lang en\|zh\|ja` | `cli-reference.md` | Bash wrapper only |
| `herdr-mcp connector` | `cli-reference.md` | Bash wrapper only |
| `herdr-mcp runtime` | `cli-reference.md` (heading/table prose) | Not a Rust subcommand |
| `herdr-mcp install` / `rollback` / `uninstall` (top-level) | Intended GA user path (G3); not yet on `origin/main` help | Tracked by parallel G3 CLI freeze; do not pretend they already shipped |
| `bin/herdr-runtime-generation ...` | `runtime-self-upgrade.md` | Separate generation helper; not `herdr-mcp` Rust CLI |
| Node `npm ci` / `node dist/server.js` as runtime | `agent-install.md`, `cli-reference.md`, contributor notes | Contributor / Edge / legacy; must not be README primary install |

### Fixed in this slice (primary install path)

README + `docs/i18n/*/install.md` + `docs/i18n/*/quick-start.md` now treat GitHub Release binary + `doctor` / `status` / `update ...` as the user install main path, and explicitly reject Node/npm-as-runtime and `service install` as the normal install instruction.
