# Runtime self-upgrade

Herdr MCP keeps the ChatGPT-facing Edge and the workstation `herdr-link` stable while local MCP runtime generations are replaced behind the link. Runtime A/B is a **same-contract-epoch** mechanism; a public contract-epoch change uses a separate supervised migration.

## Boundary

```text
ChatGPT
  -> stable Worker / OAuth / frozen public MCP contract
  -> persistent herdr-link
  -> active runtime generation
       A: http://127.0.0.1:8772/mcp
       B: http://127.0.0.1:8773/mcp
```

`herdr-link` owns the active-generation pointer. A runtime process does **not** own the public connection, so switching or stopping one runtime generation does not terminate the ChatGPT transport.

The generation manager does not start arbitrary processes. Candidate process creation is a deployment/upgrade step; after the candidate is listening on a loopback endpoint, the manager validates and switches it. This keeps process execution separate from traffic activation.

## Frozen contract profile

Production 0.3.32 uses:

```bash
HERDR_MCP_CONTRACT_PROFILE=epoch2
```

Contract epoch 2:

- exposes exactly **18 tools**;
- includes the read-only `herdr_skill` tool;
- is frozen to hash `sha256:7da23ad2ec8e7703d6380062126ba797218bde9e7711138c6b3e0ca6592efbf8`;
- fails closed if combined with `HERDR_MCP_ALL_TOOLS=1`;
- is validated from the candidate's real `tools/list` before activation.

`epoch1` remains available only as the historical **17-tool rollback/old-session compatibility profile**. It is not the current production target.

## Local control files

Production uses separate local-only files:

```text
~/.config/herdr-mcp/runtime-control-prod.json
~/.config/herdr-mcp/runtime-status-prod.json
```

They are mode `0600`. Runtime bearer credentials are **not** stored in either file; the persistent link keeps the existing local MCP credential.

A control document contains:

- monotonically increasing `revision`;
- `desired_active` generation;
- up to eight loopback generation specs;
- optional activation observation checks.

Only `http://127.0.0.0/8`, `localhost`, or `::1` candidates are accepted by the generation boundary.

## CLI

```bash
herdr-runtime-generation status
herdr-runtime-generation register --generation candidate-0.3.33-abc123 \
  --endpoint http://127.0.0.1:8773/mcp \
  --runtime-version 0.3.33
herdr-runtime-generation activate --generation candidate-0.3.33-abc123
herdr-runtime-generation rollback
herdr-runtime-generation remove --generation candidate-0.3.33-abc123
```

Set `HERDR_RUNTIME_CONTROL_PATH` and `HERDR_RUNTIME_STATUS_PATH` when operating a non-default environment such as production or a canary.

## Activation gate

Before the active pointer moves, `herdr-link` verifies the candidate through its existing runtime credential:

1. loopback endpoint is reachable;
2. health / discovery succeeds;
3. real `tools/list` succeeds;
4. tool count and canonical SHA-256 contract hash match the **current frozen epoch**;
5. optional expected runtime version matches;
6. repeated observation checks remain healthy.

A failed candidate never becomes active. `bin/herdr-self-update` applies the same rule: it requires the existing server plist to already use the current public contract profile and **refuses to perform a cross-epoch migration**.

## Switch and drain semantics

Activation is atomic at the generation pointer:

- requests already dispatched to generation A remain pinned to A and drain there;
- new requests immediately use generation B;
- the stable WSS link remains connected;
- Edge/OAuth/MCP URL and ChatGPT Connector do not change.

Rollback uses the same pointer mechanism in the opposite direction.

## Edge runtime status

Heartbeat frames carry the active runtime identity. The production Edge consumes that identity and persists version/generation changes, bypassing normal heartbeat write throttling for transitions.

Therefore `/status/<workstation>` converges to the new runtime on the next heartbeat without reconnecting the workstation link. Normal heartbeat interval is 15 seconds.

## Contract-epoch migration

A public tool-surface change is **not** an A/B runtime update. For epoch 1 → epoch 2 the safe order is:

```text
1. install/restart the local MCP server with 0.3.32 + HERDR_MCP_CONTRACT_PROFILE=epoch2
   -> direct loopback tools/list must be 18 tools and the epoch-2 hash
2. deploy the public Edge with epoch 2
   -> public tools/list becomes 18 tools including herdr_skill
   -> Edge temporarily accepts the immediately previous epoch-1 workstation link identity for rollback continuity
3. update/restart herdr-link with HERDR_CONTRACT_EPOCH=2 + the epoch-2 hash
   -> /status/<workstation> must converge to epoch 2 / runtime 0.3.32
4. verify a fresh ChatGPT conversation receives the 18-tool snapshot
```

The old epoch-1 catalog remains frozen in source and tests for compatibility evidence. It must never be silently edited to look like epoch 2.

## Safety rules

- Never activate a candidate with a different contract hash under the same contract epoch.
- Never use `herdr-self-update` to cross a contract epoch.
- Never bind a generation to a non-loopback endpoint unless the transport boundary is deliberately redesigned.
- Do not kill the old generation until its in-flight count has drained.
- Do not put runtime bearer credentials in control/status files or command output.
- A public tool-surface change is a contract-epoch operation, not a runtime upgrade.
- Keep Edge and Link upgrades separate from runtime-generation activation whenever possible.
