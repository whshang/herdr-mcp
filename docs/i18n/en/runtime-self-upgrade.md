# Runtime upgrades

*A/B upgrade the local runtime without breaking remote development.*

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

## DEV / PROD is a provenance plane over generations

Current runtimes expose an explicit source-development plane on top of the same generation machinery. It does not create a third permanent runtime environment:

```text
PROD
  published / verified installed binary
  pinned recovery source

DEV
  repo/worktree build with source provenance
  activated as a managed generation
```

For normal source dogfooding, use:

```bash
herdr-mcp dev status
herdr-mcp dev sync
herdr-mcp dev rollback
```

`dev sync` pins the current PROD binary/checksum before activating a repo-built DEV generation, and the switch is accepted only after server/Link generation reconciliation. `dev rollback` returns to that pinned PROD source instead of rebuilding source or guessing which previous generation was stable. The lower-level `bin/herdr-runtime-generation ...` commands remain useful for implementation/UAT work, but they are not the normal maintainer entry for source dogfooding.

Runtime DEV/PROD is unrelated to the browser extension's DEV/STANDALONE/STORE identities.

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

Lifecycle mutations must never use `launchctl submit`. An inferred launchd job may replay after the command exits, so a destructive rollback/update can run more than once. Use the managed lifecycle command directly from an independent process, or an explicit one-shot plist with `RunAtLoad=true` and `KeepAlive=false`.

## Activation gate

A candidate becomes active only after validation:

1. endpoint reachable;
2. health/discovery works;
3. real `tools/list` succeeds;
4. public tool contract matches the current contract epoch;
5. optional runtime version checks pass;
6. observation checks remain healthy when required.

The current first-party DEV/PROD public Edge contract is **epoch 8 / 19 actions**, while the workstation Runtime Execution Contract is **epoch 5 / 18 tools**. The immediately previous frozen epoch-4 contract and the frozen epoch-2 catalog remain only as bounded rollback/compatibility baselines; epoch 3 is historical only, and the public Edge epoch-3 identity remains a bounded non-DEV/PROD baseline. Exact build hashes are release evidence, not long-lived documentation facts. Activation follows the current frozen contract definitions.

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

### Crossing one epoch (N → N+1)

Selecting a release has two independent steps: discovery and admission.

**Discovery** answers only “which release should this workstation look at”. It reads the GitHub releases index and selects the single highest non-draft semantic version that also publishes a release manifest and that the configured update channel accepts. Discovery does not read contract identity, so it never substitutes an older release and never rewrites the choice to an intermediate bridge; the highest channel-eligible release stays the selected release.

**Admission** then validates that selected manifest before anything is downloaded. Every released updater implements admission as exact equality: the manifest `contract` identity `{epoch, hash, tool_count}` must equal the running Runtime's contract identity, otherwise the update fails closed with `release manifest contract identity mismatch`. No released v1.0.x (epoch 4) can therefore update directly to an epoch-5 release on any channel, and the result is a refusal, not a fallback to another release.

Accepting exactly one epoch forward is **planned**, not implemented in any released updater. It would be declared inside `runtime-manifest.json`:

```json
"contract": { "epoch": 5, "hash": "sha256:<epoch-5>", "tool_count": 18 },
"contract_migration": {
  "from": { "epoch": 4, "hash": "sha256:<epoch-4>", "tool_count": 18 },
  "state_schema_from": 15
}
```

The declaration would be part of the manifest bytes already covered by the existing GitHub artifact attestation of the manifest digest.

Planned admission rule, with `L` the local contract identity and `M` the manifest `contract`:

1. `M == L` — ordinary same-contract update, unchanged.
2. `M != L` — accept only when all of the following hold: `M.epoch == L.epoch + 1`; `contract_migration` is present and well formed; `contract_migration.from` equals `L` exactly (epoch, hash, tool_count); `contract_migration.state_schema_from` equals the local state schema; and `manifest.state_schema` equals the local state schema.
3. Everything else is refused before anything is installed, with an error naming the local and target epochs: an epoch skip greater than one, a lower epoch, a mismatched `from` hash or tool count, a missing or malformed declaration, or unknown extra fields inside `contract_migration`.

All other gates are unchanged and still run: manifest attestation, tag/source_ref identity, asset sha256, candidate probe, health-gated activation, update fence and channel rules. On a cross-epoch update the downloaded candidate would be probed against the target contract `M`, not `L`.

`update auto` runs the same discovery and the same admission check as the interactive command, so it may cross one planned epoch under exactly that rule and remains PROD + Stable only. This protocol does not permit a state-schema migration; schema changes still need the explicit major-migration path.

### The v1.0.4 bridge is planned and unpublished

The ordinary v1.0.x → v1.1 path is two steps: `herdr-mcp update` to the v1.0.4 epoch-4 bridge (identical contract identity, so it is an ordinary same-contract update), then `herdr-mcp update` to the first v1.1 release whose manifest declares `contract_migration` from epoch 4 (planned v1.1.0-rc.2).

v1.0.4 is a **planned, unreleased** bridge. Until it is published there is no installable bridge, and v1.1.0-rc.1 is not reachable from v1.0.x.

Once v1.0.4 is published, released v1.0.0–v1.0.3 update to it normally. Its `version` output also prints a legacy compatibility line containing `contract epoch 2 / 18 tools`, so the candidate probe those released updaters already run accepts the bridge. Both the foreground `herdr-mcp update` and the background stable scheduler (`update auto`) install it. No manual install is required.

The second step still depends on the planned `contract_migration` admission rule, which no released updater implements. v1.1.0-rc.1 does not declare `contract_migration`, so only the first v1.1 release that does (planned v1.1.0-rc.2) can be admitted from the v1.0.4 bridge.

### Rollback after an epoch crossing

`herdr-mcp rollback` re-activates the previously installed generation (for example 1.1 → v1.0.4). It is a service rollback, so the platform boundary matters:

- on macOS, a successful install/activation records a ready service rollback (the previous service definition and the previous generation) and `herdr-mcp rollback` revalidates and re-activates it;
- Linux writes no post-success service rollback record. `herdr-mcp rollback` is macOS-only.

Both sides use state schema 15, so no database restore is needed, and durable state written after the update is not rewound. Inspect the real service/state before relying on it.

- `browser_dispatches.message_canonical_digest` is a nullable additive column, not a numbered migration. A schema-15 epoch-4 binary opens the same database and ignores the column; rows written before it stay `NULL` and reconcile raw-only, which is fail-closed rather than a fabricated canonical match.
- 1.1 re-registration no longer overwrites an explicit webchat-control deny, so rollback to epoch 4 never widens permissions. The fix is in progress on [PR #605](https://github.com/whshang/herdr-mcp/pull/605).

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

## Upgrading an existing v0.4.8 installation to 1.0

For an ordinary enrolled user, the supported path is intentionally one command:

```bash
herdr-mcp update
```

The user does **not** download a 1.0 binary manually, run `update major-apply`, recreate the Worker, re-pair devices, or re-add the ChatGPT Connector. The immutable v0.4.8 updater first discovers the schema-5 v0.4.9 migration bridge. That bridge runs only as the original update job's detached worker; it is not installed as the production service. It downloads and attests the compatible schema-15 / Runtime Contract epoch-4 Runtime, invokes that Runtime's qualified major migration while the exact v0.4.8 service is still the source, and then asks the new Runtime to reconcile the already-proven Cloudflare Worker in place.

The Worker identity and public origin remain stable. Durable Objects, secrets, known optional bindings, enrolled devices, OAuth issuer and Connector records are retained. Cloudflare authorization may open in the browser during the same update job. If more than one accessible Cloudflare account contains the same Worker name, the update fails closed unless the non-secret `CLOUDFLARE_ACCOUNT_ID` disambiguator is supplied. If the foreground v0.4.8 updater reaches its bounded watch timeout while authorization is still pending, the detached migration continues; a second update command is not required.

The bridge keeps the exact v0.4.8 binary and schema-5 database snapshot as N-1 rollback material before migration. `herdr-mcp update major-rollback` remains the explicit recovery command if the migrated Runtime must be reverted. `update major-apply` remains an operator-level primitive used by the bridge and recovery/UAT workflows, not the normal v0.4.8 user instruction.

This one-command contract covers the schema-5 v0.4.8 → 1.0 boundary only. The planned v1.0.4 epoch-4 bridge is a different boundary: once that unpublished release exists, released v1.0.0–v1.0.3 install it with ordinary foreground `herdr-mcp update` and the background stable scheduler (`update auto`). See [the v1.0.4 bridge section](#the-v104-bridge-is-planned-and-unpublished).

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
