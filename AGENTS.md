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

A user-level symlink such as `~/.local/bin/herdr-mcp -> <repo>/bin/herdr-mcp` is a migration exception, not the target architecture. Do not create new repository-linked user entrypoints or rely on that link as evidence of the installed runtime version. Once the Rust CLI owns the required user-facing commands, migrate the stable user entrypoint to the installed runtime and remove the repository dependency.

### Current-state note

At the time this policy was introduced, production service ownership had already moved to the Rust generation model, while the user CLI still used the repository Bash wrapper as a compatibility bridge. This note is historical context only. Always re-run the read-only preflight above before acting on live runtime state.
