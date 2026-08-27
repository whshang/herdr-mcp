# AGENTS.md

## Binary and runtime ownership

`herdr-mcp` has distinct source, build, installed, active-runtime, and user-entry identities. Treat them as separate objects at all times.

| Layer | Canonical location | Meaning | Allowed use |
| --- | --- | --- | --- |
| Source checkout | repository/worktree | Editable source and scripts | Development only |
| Build artifact | `target/debug/herdr-mcp` or `target/release/herdr-mcp` | Ephemeral Cargo output | Tests, local candidate runs, release preparation |
| Installed generation | `~/.config/herdr-mcp/runtime/generations/rust-<content-id>/herdr-mcp` | Immutable installed binary | Service activation, update, rollback |
| Active runtime | `~/.config/herdr-mcp/runtime/current/herdr-mcp` | Managed symlink to the active installed generation | Production launchd target and authoritative runtime CLI |
| User CLI | `~/.local/bin/herdr-mcp` | Stable command users invoke | User-facing control entrypoint; see migration note below |

### Hard rules

1. Never use a repository build artifact as the production service binary. `target/*/herdr-mcp` may be rebuilt, deleted, or changed by branch switches.
2. `dev.herdr-mcp.server` must execute the stable active-runtime path `~/.config/herdr-mcp/runtime/current/herdr-mcp`. Do not point launchd at a checkout, worktree, `target/`, `bin/`, or a fixed generation path.
3. Installed generations are immutable. Create a new content-addressed generation, validate it, then atomically switch `runtime/current`. Never overwrite an existing generation in place.
4. Do not infer the active runtime from Git `HEAD`, the current worktree, Cargo output, or a process-name heuristic. Query the active runtime and the exact launchd label.
5. Do not use legacy checks such as `pgrep -f "dist/server.js"` or `pkill -f "dist/server.js"` for Rust service lifecycle decisions.
6. Build/test operations must not modify `runtime/current`, installed generations, launchd, or the user CLI unless the task explicitly performs an install/update/cutover.
7. Rollback must reactivate a previously installed managed generation using recorded service state. Do not rebuild a binary as part of rollback.
8. Keep credentials out of source, Git history, CLI diagnostics, AGENTS.md, and non-secret state records. Preserve existing service credentials during generation changes.
9. Any change to the installer, updater, CLI, release path, or service manager must preserve these ownership boundaries and include regression coverage for them.

### Service mutation safety

Rust service mutations (`service install`, `start`, `stop`, `restart`, `rollback`, `uninstall`, and update activation) must run from an independent process/terminal. Do not run them from a managed `herdr_exec` session: restarting `dev.herdr-mcp.server` can terminate the process carrying its own control transaction. Read-only `service status` is safe from managed execution.

Destructive service/update/native-host/Link lifecycle mutations must never use `launchctl submit`. Inferred launchd jobs may replay after the command exits and can consume rollback or repeat another non-idempotent mutation. Use the managed lifecycle path or an explicit one-shot plist with `RunAtLoad=true` and `KeepAlive=false` when an independent launchd job is required.

### Link ownership (G5)

Production Link (`dev.herdr-mcp.link-prod`) and the existing Node canary
(`dev.herdr-mcp.link`) remain Node until an explicit dual-verified cutover. Do
not bootout, rewrite, or retarget those live Node Link LaunchAgents as part of
ordinary development, install, update, `link run`, or candidate soak work.

1. `herdr-mcp link run` is a foreground **candidate** only. It must not mutate
   `runtime/current` or change production Link ownership by itself.
2. `herdr-mcp link install` / `link uninstall` manage **only**
   `dev.herdr-mcp.link-rust-candidate`, with ProgramArguments =
   `~/.config/herdr-mcp/runtime/current/herdr-mcp link run`. Never point Link
   launchd at a checkout, worktree, `target/`, or a fixed generation path. Never
   unload or replace `dev.herdr-mcp.link` / `dev.herdr-mcp.link-prod`.
3. Link install/uninstall/cutover/rollback mutations follow the same
   independent-Shell rule as service mutations: never from a managed
   `herdr_exec` session; never via `launchctl submit`.
4. Before any production Link cutover, dual verification from independent Shells
   is mandatory. Code must keep health `production_ready=false` until every gate
   is true and operators explicitly seal.

Before a lifecycle mutation, capture live state instead of trusting a handoff or previous message:

```bash
# Read-only preflight
git status --short --branch
ls -l "$HOME/.local/bin/herdr-mcp"
readlink "$HOME/.config/herdr-mcp/runtime/current" || true
"$HOME/.config/herdr-mcp/runtime/current/herdr-mcp" --version
"$HOME/.config/herdr-mcp/runtime/current/herdr-mcp" service status
launchctl list | awk -v label='dev.herdr-mcp.server' '$3 == label { print $1, $2, $3 }'
```

After any lifecycle mutation, verify the active generation, exact launchd job, local health, runtime version, contract epoch/tool count, and rollback state before declaring success.

### User CLI migration rule

The historical Bash entrypoint `bin/herdr-mcp` may exist as a compatibility wrapper during the Rust migration. When it is used, Rust lifecycle operations must delegate to `~/.config/herdr-mcp/runtime/current/herdr-mcp`.

Target architecture: `herdr-mcp install` / update activation maintains `~/.local/bin/herdr-mcp` → `~/.config/herdr-mcp/runtime/current/herdr-mcp` (stable PATH entry resolves the active generation through `runtime/current`, never a git checkout).

Operator note for machines still on the repo Bash bridge: run one `herdr-mcp install` or `herdr-mcp update apply` after a release that includes this linking logic; that retargets the symlink without requiring a manual `ln`.

A user-level symlink such as `~/.local/bin/herdr-mcp -> <repo>/bin/herdr-mcp` is a migration exception, not the target architecture. Do not create new repository-linked user entrypoints or rely on that link as evidence of the installed runtime version.

### Current-state note

Always re-run the read-only preflight above before acting on live runtime state. Do not infer CLI ownership from a checkout symlink alone.
