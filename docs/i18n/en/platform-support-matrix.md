# Platform and compatibility support matrix

*Production support is a tested security boundary, not a statement that a binary happens to start.*

This page is the public 1.0 compatibility contract for Herdr-MCP. A platform is production-supported only when the table below says **Production**. Anything marked Candidate or Unsupported must not be presented as an equivalent production workstation target.

## Protocol and runtime compatibility

Herdr-MCP has several independent protocol layers. They intentionally do not share one version number.

| Layer | Current production identity | Bounded compatibility | Fail-closed rule |
| --- | --- | --- | --- |
| Public MCP client protocol | `2025-11-25`, `2025-06-18`, `2025-03-26`, `2024-11-05`, `2024-10-07` are accepted by `initialize` negotiation | ChatGPT/OpenAI discovery may advertise probe version `2026-07-28`; that probe value is not an extra `initialize` protocol | Missing or unrecognized `initialize` protocol values negotiate down to the explicit legacy baseline `2025-11-25`; no unlisted protocol is advertised as supported |
| Public Edge tool contract | epoch **3**, **19** actions | A client conversation may retain an older tool snapshot, but Edge publishes the current public catalog | Tools outside the current public contract are rejected |
| Workstation Runtime Execution Contract | epoch **2**, **18** tools | Exact frozen epoch **1**, **17** tools is accepted only as the immediately previous rollback/old-session baseline | Any other epoch/hash pair is rejected before workstation execution |
| Relay wire protocol | numeric `protocol_version = 1` | None | Missing, string-valued, or unknown protocol versions are rejected before relay state is written |
| Durable runtime state | Current binary schema; releases must declare the same rollback-compatible state schema for automatic activation | Older stores migrate forward transactionally through append-only migrations | A store newer than the running binary is refused; the automatic updater rejects a release whose state schema or runtime contract identity does not exactly match its rollback-safe activation requirements |

The current ChatGPT path is tested as a stateless Streamable HTTP client: `openai-mcp` `initialize` and `tools/list` responses use the required SSE framing, `notifications/initialized` is accepted, and Connector reconnect does not depend on preserving an obsolete `Mcp-Session-Id`. Workstation reconnect, Durable Object rehydration, heartbeat hibernation, and reconnect grace are covered independently so a browser reconnect is not treated as proof that a workstation mutation is safe to replay.

## N, N-1 and N+1 policy

`N` means the currently deployed Edge contract and the currently qualified runtime generation.

- **N → N** is the normal production path. Candidate activation checks health plus the exact runtime execution contract before traffic moves. Release update activation additionally requires the release manifest to match the local durable-state schema.
- **N-1 runtime execution** is a bounded rollback path, not a general compatibility promise. Today that means the exact frozen epoch-1 runtime contract remains accepted beside current epoch 2. The pair is validated by epoch and hash; arbitrary older catalogs are not accepted.
- **N+1 runtime or control plane** is not guessed. A future epoch, relay protocol, or durable-state schema remains incompatible until the corresponding Edge/runtime migration is explicitly shipped and qualified. Current code rejects unknown contract pairs and future durable-state schemas instead of attempting a best-effort downgrade.
- **Durable state migration is one-way during upgrade.** Each SQLite migration is append-only and transactional. Rollback is safe only while the prior binary can still read the resulting state; this is why automatic release activation requires the declared state schema to remain rollback-compatible. Runtime rollback changes execution ownership; it does not undo Git/files/service side effects already performed by a tool call.

A contract migration and an implementation update are separate operations. A patch may change runtime implementation without changing `tools/list`; adding/removing a model-visible tool requires an explicit contract epoch migration and client/tool-snapshot qualification.

## Platform support

| Platform | Status | Filesystem boundary | Process / credential boundary | Network boundary | Qualification notes |
| --- | --- | --- | --- | --- | --- |
| macOS | **Production** | Remote file/Git mutation is restricted to live managed Git roots, with read-only/write-root, dirty/busy and secret-path gates. Shell execution remains the logged-in user's shell and is **not** a sandbox. TCC-sensitive native access uses the long-lived Herdr-MCP broker responsibility boundary. | User launchd owns the managed runtime and production Link; immutable generations switch through `runtime/current`. Local browser IPC uses an owned mode-0600 Unix socket; device/runtime credentials use the macOS credential boundary. | Runtime MCP stays on loopback behind a local bearer. The workstation opens the authenticated outbound Link to Edge; no inbound workstation listener is exposed publicly. | Primary production path. TCC broker/process, generation rollback, OAuth/Connector and real ChatGPT paths have dedicated regression/UAT coverage. |
| Linux (native Debian-class host) | **Production** | Same managed-Git-root and mutation gates as macOS, enforced with normal Unix filesystem ownership/permissions. There is no macOS TCC equivalent; shell remains the workstation user's unsandboxed shell. | `systemd --user` is preferred; the managed current-user process backend is the qualified fallback when a user systemd manager is unavailable. Device credentials and owned service/control files use private per-user storage/permissions. | Same loopback runtime + authenticated outbound Link model. | Native Debian existing-fleet installation, Link/service lifecycle and updater application have been qualified. The product does not currently own a resident daily Linux update scheduler; manual/external scheduling does not change the security boundary. |
| Windows x86_64 | **Candidate — not production** | No 1.0 production claim until the Windows lane proves managed-root/path behavior on a real machine, including Windows-native path edge cases. | The native implementation from [#364](https://github.com/whshang/herdr-mcp/pull/364) is on `main` and uses current-user processes, Windows Credential Manager and a Startup-folder login entry, with process ownership fencing. Source merge and hosted CI are qualification evidence, not a production guarantee; physical promotion is tracked in [#394](https://github.com/whshang/herdr-mcp/issues/394). | Candidate follows the same loopback + authenticated outbound Link model. | Required before promotion under #394: reinstall without losing identity, Herdr recovery, simulated-login recovery, Connector/device preservation, and a final real ChatGPT → Edge → Link → Windows → Herdr read path. UNC/path behavior must be covered before any broader filesystem claim. |
| WSL | **Unsupported** | No WSL host/guest filesystem, Windows-drive mount, symlink, or permission model has been qualified as equivalent to native Linux. | No WSL-specific service manager, credential, browser IPC or host/guest process ownership boundary is claimed. | WSL/NAT/Windows-host networking is not a qualified production Link boundary. | A Linux binary starting inside WSL is not support evidence. Do not use WSL as a security-sensitive Herdr-MCP workstation target until a separate qualification explicitly defines these boundaries. |

## What “Production” does and does not mean

Production support means the Herdr-MCP-owned boundary is explicit and tested. It does not turn the workstation into a container sandbox:

- `herdr_fs_*` and managed Git operations are project-root gated;
- `herdr_exec*` intentionally executes as the workstation user and can access whatever that user can access;
- Herdr panes/agents are persistent local processes under that same workstation security model;
- Edge OAuth, device authorization and outbound Link authentication protect remote entry; they do not reduce local user privileges.

When a platform cannot provide an equivalent boundary, the correct status is Candidate or Unsupported rather than silently weakening the contract.

## Promotion checklist

A Candidate platform may move to Production only after all of these are true on the exact implementation being merged:

1. service/process lifecycle survives install, restart/login recovery and failed activation without adopting unrelated processes;
2. credentials remain private and device-bound across reinstall/recovery;
3. managed-root filesystem operations cover native path semantics, including platform-specific absolute/relative path behavior and any claimed UNC/network filesystem behavior;
4. runtime/Link remain loopback/outbound-only at the workstation boundary;
5. `initialize`, `tools/list`, reconnect and a real read-only Web-AI → Edge → Link → workstation request pass;
6. required clean-machine CI runs for that OS, and physical UAT covers boundaries that hosted CI cannot prove;
7. unsupported or unqualified sub-environments remain explicitly unsupported rather than inheriting a broader platform label.

Related reading: [Architecture](architecture.md), [Runtime upgrades](runtime-self-upgrade.md), [Troubleshooting](troubleshooting.md), and the contributor [release model](../../release-model.md).
