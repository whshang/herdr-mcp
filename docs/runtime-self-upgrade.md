# Runtime A/B self-upgrade

Herdr MCP keeps the ChatGPT-facing Edge and the workstation `herdr-link` stable while local MCP runtime generations are replaced behind the link.

## Boundary

```text
ChatGPT
  -> stable Worker / OAuth / MCP contract
  -> persistent herdr-link
  -> active runtime generation
       A: http://127.0.0.1:8772/mcp
       B: http://127.0.0.1:8773/mcp
```

`herdr-link` owns the active-generation pointer. A runtime process does **not** own the public connection, so switching or stopping one runtime generation does not terminate the ChatGPT transport.

The generation manager does not start arbitrary processes. Candidate process creation is a deployment/upgrade step; after the candidate is listening on a loopback endpoint, the manager validates and switches it. This keeps process execution separate from traffic activation.

## Frozen contract profile

A newer runtime can be tested behind an older ChatGPT contract by starting it with:

```bash
HERDR_MCP_CONTRACT_PROFILE=epoch1
```

For contract epoch 1 this profile:

- exposes exactly the frozen 17 tools;
- hides `herdr_skill` until a deliberate future contract epoch;
- restores the model-visible metadata that changed after 0.3.23;
- keeps the newer runtime implementation and behavior;
- fails closed if combined with the advanced all-tools surface.

Activation is blocked unless the candidate's real `tools/list` hashes to the exact frozen epoch-1 hash.

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
herdr-runtime-generation register --generation candidate-026 \
  --endpoint http://127.0.0.1:8773/mcp \
  --runtime-version 0.3.26
herdr-runtime-generation activate --generation candidate-026
herdr-runtime-generation rollback
herdr-runtime-generation remove --generation candidate-026
```

Set `HERDR_RUNTIME_CONTROL_PATH` and `HERDR_RUNTIME_STATUS_PATH` when operating a non-default environment such as production or a canary.

## Activation gate

Before the active pointer moves, `herdr-link` verifies the candidate through its existing runtime credential:

1. loopback endpoint is reachable;
2. health / discovery succeeds;
3. real `tools/list` succeeds;
4. tool count and canonical SHA-256 contract hash match the frozen contract;
5. optional expected runtime version matches;
6. repeated observation checks remain healthy.

A failed candidate never becomes active.

## Switch and drain semantics

Activation is atomic at the generation pointer:

- requests already dispatched to generation A remain pinned to A and drain there;
- new requests immediately use generation B;
- the stable WSS link remains connected;
- Edge/OAuth/MCP URL and ChatGPT Connector do not change.

Rollback uses the same pointer mechanism in the opposite direction.

## Edge runtime status

Heartbeat frames already carry the active runtime identity. The production Edge consumes that identity and persists it when version/generation changes, bypassing normal heartbeat write throttling for the transition.

Therefore `/status/<workstation>` converges to the new runtime on the next heartbeat without reconnecting the workstation link. Normal heartbeat interval is 15 seconds.

## Production evidence — 2026-08-23

The existing `prod-real-runtime` link first completed a real round trip without changing the public Connector:

```text
0.3.23 / stable-023
  -> 0.3.26 / candidate-026
  -> 0.3.23 / stable-023
```

After that proof, production was permanently promoted to the newer implementation without changing the public ABI:

```text
0.3.23 / stable-023 @ 8772
  -> temporary 0.3.26 / candidate-026 @ 8773
  -> restart 8772 with 0.3.26 + HERDR_MCP_CONTRACT_PROFILE=epoch1
  -> 0.3.26 / stable-026 @ 8772
```

Production now ends at `stable-026`, port 8772, runtime 0.3.26. The temporary 8773 process and old generation entries are removed. Both the transition candidate and the final stable runtime advertise exactly 17 tools with contract epoch 1 and hash:

```text
sha256:3f23083ae31b977dad21b1ec9d6919c49e1067a27f7b7eea7bdd021b54770c0d
```

A `herdr_inspect` issued from the same ChatGPT conversation observed the version changes without Connector reconfiguration. The production Edge heartbeat status converged with the active generation, and the browser-extension smoke suite passed against the final 0.3.26 server on `127.0.0.1:8772`.

## Safety rules

- Never activate a candidate with a different contract hash under the same contract epoch.
- Never bind a generation to a non-loopback endpoint unless the transport boundary is deliberately redesigned.
- Do not kill the old generation until its in-flight count has drained.
- Do not put runtime bearer credentials in control/status files or command output.
- A public tool-surface change is a contract-epoch operation, not a runtime upgrade.
- Keep Edge and Link upgrades separate from runtime-generation activation whenever possible.
