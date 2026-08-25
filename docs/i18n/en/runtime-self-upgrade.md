# Runtime A/B: upgrade the local runtime without breaking remote development

herdr-mcp separates the public connection plane from the local runtime plane.

For ChatGPT, the Edge origin, OAuth identity and MCP URL should remain stable. On the workstation, the herdr-mcp runtime can be upgraded, validated, switched and rolled back without reconnecting the Connector.

```text
ChatGPT
  │ stable MCP/OAuth origin
  ▼
Cloudflare Edge
  │ persistent workstation WSS
  ▼
herdr-link
  │ active generation pointer
  ├───────────────┐
  ▼               ▼
runtime A       runtime B
127.0.0.1:8772 127.0.0.1:8773
```

## The problem it solves

Without separation, a local upgrade can affect the public path, OAuth identity, in-flight tool calls and ChatGPT sessions at the same time.

A/B deployment separates:

- building a candidate runtime;
- validating that runtime;
- sending new traffic to it;
- draining the previous runtime.

## What is a runtime generation

A generation is one independently addressable local herdr-mcp runtime:

```text
generation A
  current stable runtime

generation B
  candidate runtime
```

`herdr-link` owns the active-generation pointer. Edge does not need to know local port changes.

Therefore:

```text
runtime switch != Connector switch
```

## A/B is not a process manager

The generation manager does not start arbitrary commands. The recommended flow is:

1. build candidate;
2. start candidate on a new loopback endpoint;
3. register generation;
4. run health and contract checks;
5. activate;
6. observe;
7. remove old generation later.

Process creation and traffic activation stay separate.

## CLI

```bash
bin/herdr-runtime-generation status

bin/herdr-runtime-generation register \
  --generation candidate-<id> \
  --endpoint http://127.0.0.1:8773/mcp \
  --runtime-version <version>

bin/herdr-runtime-generation activate --generation candidate-<id>

bin/herdr-runtime-generation rollback

bin/herdr-runtime-generation remove --generation candidate-<id>
```

Start with `status` before any upgrade or rollback. Do not infer the active runtime from an old deployment log.

## Activation gate

A candidate becomes active only after validation:

1. endpoint reachable;
2. health/discovery works;
3. real `tools/list` succeeds;
4. public tool contract matches the current contract epoch;
5. optional runtime version checks pass;
6. observation checks remain healthy when required.

The current public contract is **epoch 2 / 18 tools**. Exact build hashes are release evidence, not long-lived documentation facts. Activation follows the current frozen contract definition.

## Runtime upgrade and contract migration are different

Runtime A/B means:

> replace implementation while keeping the public contract.

Examples:

- fix filesystem behavior;
- improve snapshot fallback;
- improve execution reliability;
- change internal relay behavior without changing tools.

A tool surface change is different:

```text
runtime implementation upgrade
    !=
public MCP contract migration
```

Contract migrations affect ChatGPT tool snapshots, Edge compatibility and Link expectations. They require an explicit epoch migration process.

## Switching traffic

Activation changes the local routing pointer:

```text
before
new requests → A

activate B

after
new requests → B
existing requests → A drains
```

The desired result:

- persistent WSS remains connected;
- Edge URL remains unchanged;
- OAuth identity remains unchanged;
- already delivered work is not executed twice.

## Rollback

Rollback returns new requests to a previous known-good generation:

```bash
bin/herdr-runtime-generation status
bin/herdr-runtime-generation rollback
```

Runtime rollback is not business rollback. It does not undo Git changes, files, remote services or Agent side effects already performed by a previous runtime.

## Local control state

Generation state stores:

- generation specifications;
- desired active generation;
- observed active generation;
- previous/last-good generation;
- activation observations.

It is workstation control state, not repository source, and does not store bearer credentials.

## Heartbeats and Edge state

The workstation link reports active runtime identity through heartbeat data.

Edge can therefore observe:

```text
workstation online
active generation changed
runtime version changed
```

A short delay while heartbeat state converges does not necessarily mean activation failed. Verify local generation state and subsequent heartbeat updates.

## Recommended upgrade flow

```text
Inspect current state
  ↓
Build candidate
  ↓
Start candidate
  ↓
Register generation
  ↓
Validate health + contract
  ↓
Activate
  ↓
Observe real usage
  ↓
Remove old generation later
```

If a step fails:

- before activation: fix candidate;
- after activation: evaluate rollback;
- uncertain mutation delivery: inspect first, never blindly repeat.

## `herdr-self-update`

`bin/herdr-self-update` uses the generation mechanism.

It is suitable for:

- same contract epoch updates;
- candidate validation and controlled switching.

It is not a shortcut for:

- public contract changes;
- Edge/OAuth migration;
- Domain/DNS changes;
- unrelated Herdr daemon upgrades.

Those are separate release planes.

## Release planes

```text
Public Edge plane
Worker / Durable Object / OAuth / public MCP relay

Local runtime plane
herdr-link / runtime generation
```

Keeping them separate reduces the blast radius of releases.

## Security rules

- candidates must stay on loopback endpoints;
- contract mismatches cannot become active;
- delivered mutations are never duplicated because of a switch;
- old generations drain before removal;
- credentials stay outside generation state;
- inspect real Git/Agent/service state before and after rollback;
- contract migration and domain mutation are not normal self-update operations.

## Acceptance criteria

A successful A/B upgrade proves:

- candidate healthy;
- contract gate passed;
- active generation changed;
- Edge heartbeat converged;
- new requests use the candidate;
- Connector/OAuth identity remained stable;
- a real MCP call succeeded;
- rollback target remained available during observation.

Related:

- [Cloudflare Edge deployment](cloudflare-edge-deployment.md)
- [CLI reference](cli-reference.md)
- [Troubleshooting](troubleshooting.md)
- [Architecture](architecture.md)
