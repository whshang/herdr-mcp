# Link production cutover contract

This is the durable safety contract for moving the production Herdr Link to the native runtime. Historical GA rehearsal logs and machine-specific evidence are intentionally not retained here.

## Production ownership invariants

Production readiness is derived from `herdr-mcp link status` and must remain fail-closed. The production path must not point to a repository checkout, build directory, or ad-hoc process. The active production LaunchAgent must execute the installed `runtime/current/herdr-mcp link run` path.

The required gates are:

| Gate | Requirement |
| --- | --- |
| `rust_cli_link_run` | installed runtime supports `herdr-mcp link run` |
| `launchd_prod_program_is_rust_runtime` | production LaunchAgent uses `runtime/current/herdr-mcp link run` |
| `launchd_not_repo_checkout` | production ownership never points into a checkout/worktree |
| `runtime_control_generation_rust_compatible` | desired and active generation use the supported native runtime contract |
| `health_runtime_not_candidate` | health metadata represents production ownership, not a candidate state |
| `user_cli_not_repo_bash_bridge` | user CLI resolves through the installed runtime |
| `node_link_not_required` | production path no longer depends on the legacy Node Link |
| `dual_verification_uat` | an independent operator verification has been explicitly recorded |

`production_ready` must remain false until every gate is true and the operator explicitly seals the cutover. Code must never infer the independent verification gate from process state alone.

## Cutover transaction

`herdr-mcp link cutover --execute` is a bounded ownership transaction:

1. validate all preconditions and preserve the currently recoverable production configuration;
2. back up the production Link LaunchAgent configuration;
3. activate only the intended production owner using the installed runtime path;
4. verify ProgramArguments, launchd loaded state, runtime health and Link status;
5. on activation or verification failure, restore the previous production configuration;
6. keep unrelated development/candidate Link owners untouched;
7. do not mark `production_ready` merely because activation succeeded.

Cutover/rollback actions that affect production ownership must be initiated from an independent operator shell rather than from a Herdr-managed execution session whose connectivity depends on the path being changed.

## Runtime-control migration

Runtime-control migration and LaunchAgent ownership are separate concerns. Preparing or applying a compatible runtime-control generation does not by itself constitute a production cutover and must not set `production_ready`.

## Rollback

Rollback must restore the previous known-good production owner from the preserved backup, verify that it is loaded and reachable, and leave release generations intact. Never delete the active or rollback generation as part of cutover cleanup.

## Secrets

Link credentials and runtime authentication material remain in their designated protected stores. Cutover tooling may preserve or reference them but must never print, migrate into repository files, or encode secrets into documentation/evidence.
