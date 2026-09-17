# Herdr 0.9.1 multi-machine and dual-path control

*Use Herdr saved SSH machines and Herdr-MCP Edge devices together without confusing their identities.*

Herdr 0.9.1 and Herdr-MCP solve different parts of multi-machine control:

| Control plane | Identity | Transport | Best use |
| --- | --- | --- | --- |
| Herdr saved machine | machine profile id + SSH target + Herdr session | SSH / Herdr remote client | Human multi-machine TUI, maintenance, bootstrap, UAT, recovery |
| Herdr-MCP Edge device | immutable `dev_*` `device_id` + device credential | outbound Link → Edge → MCP | ChatGPT/Web-AI routing, device affinity, delivery evidence |

The same physical workstation can have both identities. Do not merge them. A matching label or hostname is not proof that two records are the same workstation.

## What is shared when both paths reach the same session

If the saved SSH machine and the Edge device both reach the same Herdr server and Herdr session, they operate the same live resources:

- workspace/tab/pane topology;
- agents and agent state;
- the actual PTY/foreground processes;
- terminal output;
- Git working trees and filesystem state.

This is not replication or eventual synchronization. Both control paths are talking to the same Herdr server/session. A change made through one path is visible through the other when it is read again.

If the SSH profile specifies a different Herdr session, the Herdr workspace/pane state is independent even on the same physical host.

Edge-only state is not shared with the saved machine profile: `device_id`, device credential, Link online/offline state, runtime generation, reconnect evidence, and mutation delivery state remain Herdr-MCP concerns. Saved-machine profile id, SSH target, session, enabled state, and TUI selection remain Herdr client concerns.

## Never address a machine by a bare workspace or pane id

Herdr server ids are server/session scoped. Two machines can both have `w1`, `w1:t1`, and `w1:p1` at the same time.

For Edge work, keep the `device_id` or device-bound `herdr_ref_*` with the workspace/pane reference. For saved-machine work, keep the machine profile + Herdr session with the workspace/pane id. Do not cache or pass around a bare `w1:p1` as though it were globally unique.

Herdr 0.9.1 fixes the client-side same-id filtering tracked by issue [#3732](https://github.com/herdrdev/herdr/issues/3732). Workspace and pane ids are still scoped to one Herdr server/session, so Herdr-MCP keeps device affinity explicit even when a saved machine and an Edge device reach the same workstation.

## Native machine-scoped CLI in Herdr 0.9.1

Herdr 0.9.1 adds native CLI forwarding to a saved SSH machine:

- `herdr --machine <label-or-id> <command>` runs supported API commands against that saved machine's configured Herdr session without requiring an open remote Herdr window;
- workspace, worktree, tab, pane, and agent commands can be addressed this way;
- update Herdr on both machines before relying on CLI forwarding;
- a failed remote command stays failed and never falls back to Local;
- `herdr --remote <target>` remains the interactive remote-TUI attach path.

Use the saved profile as the routing identity and discover ids on that remote server before mutation:

```bash
# Discover the saved profile. Keep its id, target and session together.
herdr machine list --json

# Route API commands through the saved machine profile.
herdr --machine <label-or-id> workspace list
herdr --machine <label-or-id> pane list --workspace <remote-workspace-id>
herdr --machine <label-or-id> agent list
```

Do not assume ids obtained from the local server are valid remotely. For a pre-0.9.1 endpoint that must be bootstrapped or repaired, explicit SSH execution remains a compatibility/recovery path; upgrade the remote endpoint and return to `--machine` for normal programmatic control.

The upstream multi-machine Ideas thread is [Discussion #515](https://github.com/herdrdev/herdr/discussions/515). The saved-machine profile remains an SSH/Herdr identity; native `--machine` forwarding does not merge it with the Herdr-MCP Edge `device_id` namespace.

## Which path should ChatGPT use?

When a workstation is enrolled as an Edge device, ChatGPT/Web-AI operations normally use the Edge path. It provides immutable device identity, generation fencing, reconnect state, and mutation-delivery evidence.

Use the saved-machine/SSH path explicitly for maintenance, first-time bootstrap, Debian/Linux UAT, or recovery when that is the requested transport. Do not silently fail over a mutating Edge call to SSH:

- `delivery_state=not_delivered`: after connectivity/state verification, a reissue through an explicitly chosen path may be safe;
- `delivery_unknown`, delivered/uncertain state, or missing delivery evidence: inspect live pane/Git/runtime/resource state first and do not replay the mutation blindly.

Herdr TUI machine selection never changes the target of an Edge call. An Edge call stays bound to its explicit/default Herdr-MCP device and any returned `herdr_ref_*` affinity.

## Verify that two paths really share one Herdr base

For a controlled test workstation, use read-only evidence first:

1. Confirm the saved-machine profile points at the intended SSH target and Herdr session.
2. Confirm the Edge fleet contains the intended immutable `device_id` and that it is online.
3. Read the workspace/pane through both paths.
4. Compare `pane.process_info` from both paths. Matching pane id, shell PID/foreground process group, executable, and cwd is strong evidence that both paths address the same PTY rather than two coincidentally named panes.
5. If needed, write a harmless marker in an idle test shell through one path and read it through the other, then repeat in the opposite direction.

Do not use a production or busy agent pane for a marker test, and do not use pairing/revoke merely to prove that two existing control paths share state.
