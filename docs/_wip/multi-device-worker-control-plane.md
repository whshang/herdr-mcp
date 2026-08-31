# Herdr Multi-Device Worker Control Plane

Status: **v0.4.3 core design frozen for implementation**
Baseline: `origin/main` at branch creation (`1f8f9cf`, after v0.4.2)
Scope owner: Cloudflare Worker control plane + Rust Link compatibility

Implementation status (2026-08-31):

- [x] Public Edge Contract and Runtime Execution Contract are separated in code.
- [x] `DeviceRegistryDO` and canonical `dev_<ULID>` identity exist.
- [x] The configured legacy default workstation bootstraps one stable registry device on Link connect without heartbeat writes.
- [x] Public epoch 3 exposes Edge-local `herdr_devices`; Runtime execution remains epoch 2.
- [x] Workstation-bound public tools carry one common Edge-only `device` selector; Edge resolves once, strips it, then forwards Runtime v2 args.
- [x] Explicit ID/name routing, single-routable-device selection, legacy-empty-registry fallback, and `device_ambiguous` fail-closed behavior are covered by tests.
- [x] Per-device credential and one-time enrollment are implemented in the Worker/Registry and macOS Rust CLI, including single-use expiry, credential-to-device binding, self-revoke compensation, and native Keychain persistence.
- [ ] Real two-device UAT is still pending until the secure enrollment Edge is
  deployed to production and a real second Mac joins the same deployed
  Worker/Connector; second-device support is not release-qualified before that
  UAT passes.
- [ ] Device-aware opaque workspace/pane refs, scheduling mutations, and Web Control Console remain later phases.
- [x] Routing authority boundary frozen: browser conversation/project binding is local continuity/UI metadata only and can never be an authoritative Edge routing input (current ChatGPT MCP transport is sessionless and provides no trusted per-conversation/project identity).

## 1. Product decision

Herdr v0.4.3 evolves the public control plane from one Worker targeting one workstation into one Worker routing to multiple user devices:

```text
ChatGPT / CLI
      |
      v
one Herdr Connector
      |
      v
one Cloudflare Worker
      |
      +-- Device Registry
      +-- request-scoped Device Router
      +-- per-device authentication
      |
      +-- Device A -> WorkstationDO(A) -> herdr-mcp
      +-- Device B -> WorkstationDO(B) -> herdr-mcp
      `-- Device C -> WorkstationDO(C) -> herdr-mcp
```

Frozen product rules:

1. Ordinary multi-device use keeps one stable Connector URL.
2. One Worker can register and route to N devices.
3. Public terminology is `device`; relay/runtime internals may continue using `workstation`.
4. Device addition/removal does not change the Connector URL or OAuth issuer.
5. Runtime installation and Worker enrollment are separate lifecycle operations.
6. Multi-device correctness must reuse the existing WorkstationDO delivery, retry and mutation-safety rules.
7. The first multi-device release prefers deterministic selection and fail-closed ambiguity over heuristic routing.
8. Web Control Console productization does not block the core multi-device release.

## 2. Current v0.4.2+ baseline

The implementation baseline is already Rust production runtime with contract epoch 2 and 18 runtime tools.

Post-v0.4.2 changes already on `main` must be treated as existing behavior:

- oversized outbound results are contained without turning the Link into an outage;
- prolonged `workstation_offline` recovery exists;
- public workstation retry policy exists;
- Browser Control Center owns local workspace/pane/agent/terminal control;
- image-only ChatGPT turns are captured correctly;
- extension distribution is converging on DEV / STANDALONE / STORE;
- runtime installation remains DEV / PROD.

The multi-device implementation must extend these contracts rather than replace them.

## 3. Identity model

### 3.1 Worker

A Herdr Worker is the user's public control plane.

Stable fields:

```text
worker_id
worker_name
origin
public_contract_identity
created_at
```

One user may intentionally deploy multiple Workers for isolation, but one Worker is the ordinary multi-device path.

### 3.2 Device

A device is a user-visible remote computing device.

```text
device_id = dev_<ULID>
name      = mutable alias
```

Frozen invariants:

- `device_id` is immutable and Worker-scoped;
- names may be changed and may collide;
- names are never security identities;
- browser extension channel changes do not change `device_id`;
- runtime generation/version changes do not change `device_id`;
- credential rotation does not change `device_id`.

For newly enrolled v0.4.3 devices, the preferred mapping is:

```text
device_id == workstation_id
```

Legacy installations may retain an existing workstation id:

```text
device_id      = dev_01...
workstation_id = prod-real-runtime
```

The registry owns this mapping during migration.

### 3.3 Instance

`HERDR_MCP_INSTANCE` continues to describe a local runtime instance on one physical device. Multi-instance remote scheduling is outside the v0.4.3 core.

## 4. Device state model

Device state is intentionally multi-dimensional.

### Connection

```text
online
reconnecting
offline
stale
```

Authority: authenticated WebSocket presence + WorkstationDO runtime state.

### Scheduling

```text
enabled
draining
paused
```

Authority: Device Registry desired state.

### Authorization

```text
active
suspended
revoked
```

Authority: Device Registry credential/authorization state.

These states must never collapse into one ambiguous `status` field.

## 5. Public contract and runtime contract

This separation is a v0.4.3 P0 requirement.

Current v0.4.2 shape:

```text
Public Edge Contract == Runtime Link Contract == epoch 2
```

Target shape:

```text
Public Edge Contract v3
  - ChatGPT-visible
  - device-aware
  - includes Edge-local fleet tools
  - wraps the existing runtime tools with routing metadata

Runtime Execution Contract v2
  - existing 18 tools
  - Link-visible
  - Rust execution semantics unchanged
  - no fleet administration responsibility
```

Compatibility policy:

- runtime epoch 2 remains the normal v0.4.3 execution contract;
- runtime epoch 1 remains only the existing bounded rollback compatibility baseline;
- public contract evolution must not require all connected runtimes to implement Edge-local tools;
- Link compatibility checks use the Runtime Execution Contract, never the Public Edge Contract.

## 6. Public routing extension

The public contract may expose a common Edge-only device selector on workstation-bound tools.

Conceptually:

```json
{
  "device": "dev_01...",
  "action": "status"
}
```

The implementation must treat this as a common Public Routing Extension generated/wrapped at the Edge boundary. It must not edit the canonical 18-tool runtime schemas individually.

Before forwarding to the runtime:

1. resolve the public selector to one immutable `device_id`;
2. authorize the selected device;
3. verify scheduling/routability;
4. map to `workstation_id`;
5. remove Edge-only routing metadata;
6. send the existing epoch-2 runtime request.

## 7. Deterministic routing

Device routing is request/operation-scoped. There is no Worker-global `current_device`.

v0.4.3 routing priority:

1. explicit device selector;
2. device-aware opaque ref from an earlier result;
3. exactly one routable device;
4. otherwise return `device_ambiguous`.

Browser conversation/project binding is **local continuity/UI metadata only**. It may *suggest* a device, but it is never authoritative routing input: the current ChatGPT/OpenAI MCP transport is intentionally sessionless (`mcp-chatgpt-transport.ts` issues/requires no `Mcp-Session-Id`), so Edge receives no trusted per-conversation/project identity. `resolveDeviceRouteWithContext()` deliberately ignores caller-controlled binding args, and `device-refs.ts` strips `binding_device_id` / `__herdr_binding_device_id` / `herdr_binding` before forwarding. Therefore v0.4.3 MUST NOT claim trusted browser conversation/project binding as an Edge routing authority.

Explicitly forbidden as routing input:

- Worker-global `current_device`;
- OAuth-client-wide sticky device;
- probing devices for matching paths;
- trusting raw browser/model-supplied `binding_device_id`.

Future trusted binding requires a server-verifiable per-conversation/project identity/token delivered through a trusted channel; the current sessionless ChatGPT MCP transport does not provide one.

Deferred heuristics:

- probing every device for a project path;
- model-selected device based on path strings;
- capability scoring/load balancing;
- CPU/GPU-aware scheduling;
- mutation failover to another device.

### Retry device affinity

Once an operation selects a device, that selection is immutable for the operation lifetime:

```text
request -> selected device -> operation -> retry/recovery on same device
```

A failure on Device A must never cause the same mutation to be automatically replayed on Device B.

## 8. Device-aware refs

Workspace, pane and other opaque refs returned through the public control plane must progressively carry device identity.

Conceptual shape:

```json
{
  "device_id": "dev_01...",
  "workspace_id": "w3Y",
  "pane_id": "w3Y:p1"
}
```

Consumers must use the opaque/device-aware identity instead of inferring a device from an absolute path.

Opaque refs are the only device-carrying identity the Edge trusts for follow-up routing. Caller-supplied binding keys (`binding_device_id`, `__herdr_binding_device_id`, `herdr_binding`) are stripped by `device-refs.ts` and never carry routing authority.

Browser local-project auto-bind work in v0.4.3 must leave room for this `device_id` dimension even if its first implementation operates only on the local device.

## 9. Device Registry

Add one Worker-side durable registry for identity and desired state.

Authority:

```text
device_id
workstation_id
name
authorization
scheduling
credential metadata
enrolled_at
updated_at
revoked_at
stable labels/capability summary (later)
```

The registry must not own heartbeat hot-path state.

Writes occur only for meaningful lifecycle changes:

```text
enroll
rename
pause/resume
suspend/unsuspend
credential rotate
revoke
```

Realtime connection/health remains in `WorkstationDO(device/workstation)`.

## 10. Device authentication

Formal multi-device enrollment must not reuse one global Link shared secret as the identity of every new device.

v0.4.3 minimum contract:

```text
Device A -> credential A -> only Device A
Device B -> credential B -> only Device B
```

A first implementation may use a high-entropy random per-device credential:

```text
device stores secret securely
Worker stores credential identifier + verifier/hash
credential is bound to immutable device_id
```

Public-key device identity can be introduced later without changing `device_id`.

`LINK_SHARED_SECRET` remains a legacy compatibility path for existing single-device deployments. It is not the normal enrollment mechanism for adding a second v0.4.3 device.

## 11. Pairing

Runtime installation stays independent:

```text
herdr-mcp install
herdr-mcp doctor
```

Worker onboarding is explicit and uses only the implemented P0-C pairing CLI:

```text
herdr-mcp worker pair [--ttl-seconds 600] [--name NAME]
herdr-mcp device pair [same options]
herdr-mcp worker connect <pairing-address> [--name NAME]
```

A richer onboarding tree (`worker setup`, `worker create`, `worker status`,
`devices list`) remains a later product surface (Section 13).

For an existing Worker, the intended deterministic path is:

```text
owner starts a short-lived pairing session (worker pair)
       |
       v
pairing address + 6-digit code are shown to the user
       |
       v
new device: herdr-mcp worker connect <pairing-address>  (code entered interactively)
       |
       v
stable device_id + per-device credential
       |
       v
Link online on the same Worker
```

Pairing creation authority:

- only the owner/default workstation creates pairings; the Edge accepts
  pairing creation only from the Edge MCP (OAuth owner) identity or from the
  configured `DEFAULT_WORKSTATION_ID` presenting its production Link credential
  (`authenticateEnrollmentCreator`);
- the CLI creator path resolves the production Link identity and its Keychain
  credential before it will call the Edge;
- member devices cannot recursively create further pairings. A joining device
  receives only the ability to consume a one-time pairing; it never gains
  pairing-creation authority.

Pairing code requirements:

- six decimal digits (000000..999999, leading zeros allowed), CSPRNG-generated;
- single use;
- short TTL (default 600s, max 600s);
- Worker-bound;
- device-pairing scope only;
- not persisted in normal logs;
- not equivalent to the final device credential;
- never accepted as a command-line argument;
- limited to 5 wrong attempts before the session is permanently locked;
- Cloudflare deployment credentials are not required on the joining device.

Pairing id requirements:

- high entropy (>=128 random bits) and unguessable;
- carried in the pairing address URL fragment, not in HTTP access-log paths;
- never stored raw; the Edge stores only a verifier bound to the raw pairing
  id + code + Worker context;
- redacted from logs.

Current macOS credential contract:

- the joining device receives the final 256-bit random credential once;
- the Worker persists only its verifier/hash;
- Rust writes the final credential directly through Security.framework into macOS Keychain;
- `edge.device_id` is non-secret config and selects a deterministic per-device Keychain service;
- production Link reconciliation updates `HERDR_WORKSTATION_ID` and `HERDR_LINK_KEYCHAIN_SERVICE` without putting the credential in the plist, argv, shell history, or logs;
- if Keychain persistence fails after pairing consumption, the client uses the just-issued credential only to revoke that same device as compensation.

The current secure joining CLI is macOS-only because the credential backend is Keychain. Other platforms must gain an equivalent OS credential store before their `worker connect` path may consume a one-time pairing.

## 12. Edge-local MCP tools

### `herdr_devices`

Read-only fleet discovery, executed at the Edge rather than forwarded to a workstation.

Minimum output:

```json
{
  "devices": [
    {
      "device_id": "dev_01...",
      "name": "macbook-main",
      "authorization": "active",
      "connection": "online",
      "scheduling": "enabled",
      "health": "healthy",
      "runtime_version": "0.4.2"
    }
  ]
}
```

### `herdr_device`

Administration tool is P1 after read-only fleet + secure routing are stable.

Candidate actions:

```text
status
rename
pause
resume
suspend
unsuspend
revoke
create_enrollment
```

Mutation semantics must share one Device Administration Service with CLI and future Web Console.

## 13. CLI boundary

Implemented P0-C command surface:

```text
herdr-mcp worker pair [--ttl-seconds 600] [--name NAME]
herdr-mcp device pair [same options]
herdr-mcp worker connect <pairing-address> [--name NAME]
```

The pairing code is intentionally absent from argv and normal stdout. `worker pair` reports the pairing address, the 6-digit code (formatted `123 456`), and expiry only. `worker connect` reads the code from a no-echo TTY prompt (or a single non-echo stdin line when noninteractive).

Later command tree:

```text
herdr-mcp worker setup
herdr-mcp worker create
herdr-mcp worker status
herdr-mcp worker doctor

herdr-mcp devices list
herdr-mcp device status <device>
```

P1 administration:

```text
herdr-mcp device rename ...
herdr-mcp device pause ...
herdr-mcp device resume ...
herdr-mcp device suspend ...
herdr-mcp device unsuspend ...
herdr-mcp device revoke ...
herdr-mcp device enrollment create
```

All mutations resolve aliases to immutable `device_id` before execution.

## 14. Worker Web Control Console

The Console is a fleet control plane, not a replacement for the existing Browser Control Center and not a general RMM product.

Core release does not depend on a full Console.

P2 minimal Console scope:

```text
Overview
Devices
Enrollment
Worker diagnostics
```

Explicitly outside the core:

- remote desktop;
- generic remote terminal;
- full file browser;
- service manager;
- patch/software inventory;
- long-term analytics.

Local workspace/pane/agent/terminal control remains in the existing Browser Control Center and Herdr MCP tools.

## 15. Durable Objects and quota rules

Required core dependency:

```text
Cloudflare Worker
DeviceRegistryDO
WorkstationDO x N
OAuthStoreDO
```

Core must not depend on D1, Analytics Engine or Queues.

Quota/correctness rules:

```text
heartbeat registry writes      = 0
status poll writes             = 0
Web Console refresh writes     = 0
safe read observation writes   = 0 whenever correctness allows
mutation correctness state     = protected
telemetry                      = bounded and shed-able
```

Realtime heartbeat remains in WorkstationDO. The existing Link reliability/recovery logic remains authoritative.

## 16. Privacy and activity boundary

Core correctness does not require long-term invocation history.

Any Recent Activity implementation must remain bounded and must not store by default:

- full tool arguments;
- shell command text;
- file contents;
- tool result bodies;
- ChatGPT conversation text;
- secrets or enrollment material.

Analytics is optional and outside the multi-device P0 dependency chain.

## 17. Public error model

Public errors use device terminology while preserving existing internal workstation delivery semantics.

Target public codes include:

```text
device_not_found
device_ambiguous
device_offline
device_reconnecting
device_paused
device_suspended
device_revoked
device_unhealthy
device_contract_incompatible
```

Edge maps internal workstation errors to the stable device-facing boundary and retains retry metadata where applicable.

## 18. Implementation phases for v0.4.3

### P0-A — Protocol foundation

- split Public Edge Contract from Runtime Execution Contract;
- freeze `device_id` validation/format;
- add Device Registry model;
- preserve legacy workstation mapping;
- add selected-device request context;
- add device-facing error mapping;
- lock retry device affinity in tests.

### P0-B — Read-only fleet

- persistent DeviceRegistryDO;
- legacy current device bootstrap/migration;
- aggregate Registry + WorkstationDO presence;
- add Edge-local `herdr_devices`;
- expose health/version/connection/scheduling/authorization;
- preserve single-device behavior.

### P0-C — Secure enrollment

- [x] one-time enrollment contract;
- [x] per-device credential;
- [x] macOS second-device connect flow with native Keychain persistence;
- [x] credential-to-device binding;
- [x] self-revoke/compensation path;
- [x] retain legacy shared-secret compatibility for the configured default workstation only;
- [ ] real Device A + Device B deployed UAT before release qualification.

### P0-D — Explicit execution routing

- public routing extension;
- exact `device_id` selection;
- device-aware refs;
- browser conversation/project binding handled strictly as local continuity/UI metadata (may suggest a device; never authoritative routing input);
- implicit selection only when exactly one device is routable;
- `device_ambiguous` otherwise;
- no cross-device retry/failover.

### P1 — Scheduling and administration

- pause/drain/resume;
- rename;
- suspend/unsuspend;
- `herdr_device`;
- CLI mutation commands;
- admin audit.

### P2 — Console/productization

- minimal Web Console;
- Enrollment UI;
- diagnostics;
- bounded Recent Activity;
- docs/site integration;
- broader UAT.

## 19. v0.4.3 core UAT

The formal two-device UAT tests only **server-provable routing**. It never treats browser conversation/project binding as routing authority (that is local continuity/UI metadata only). The feature is considered functionally real when all of the following pass:

```text
Device A and Device B are simultaneously online
same Worker
same Connector
herdr_devices returns both
explicit request to A executes only on A (explicit A/B isolation)
explicit request to B executes only on B (explicit A/B isolation)
opaque ref keeps its device identity (opaque-ref affinity)
unqualified mutation with multiple candidates fails device_ambiguous
retry/recovery stays on the originally selected device (retry stickiness)
credential A cannot authenticate as Device B (per-device credential isolation)
revoked Device B credential cannot reconnect (per-device revocation)
no DeviceRegistry heartbeat churn from heartbeat/status/UI observation
legacy v0.4.2 single-device deployment still works during upgrade
```

Additional safety gates:

- telemetry failure cannot alter tool correctness;
- no heartbeat-driven Device Registry writes;
- Public Contract upgrade does not force a Rust runtime tool-surface upgrade;
- browser extension DEV/STANDALONE/STORE switching does not change device identity;
- local state schema compatibility work remains independent from Worker Registry storage;
- the v0.4.2 -> v0.4.3 candidate -> rollback v0.4.2 -> candidate re-apply UAT is schema-neutral; there is no 5 -> 4 state-schema downgrade in the v0.4.2/v0.4.3 path (immutable tag `v0.4.2` and current `main` both carry `SCHEMA_VERSION = 5`).

## 20. Explicit non-goals

v0.4.3 core does not implement:

- team RBAC / organization multi-tenancy;
- cross-device mutation failover;
- automatic agent-session migration;
- shared filesystem abstraction across devices;
- generic load balancing;
- CPU/GPU scheduler;
- full RMM console;
- long-term analytics as a required dependency;
- complete rename of internal `workstation_*` protocol fields;
- Worker-global current device state;
- OAuth-client-wide sticky device;
- probing devices for matching paths;
- trusting raw browser/model-supplied `binding_device_id` as routing input.

## 21. Frozen implementation principle

The central invariant for v0.4.3 is:

```text
Public request
  -> deterministically select one immutable device_id
  -> authorize that device
  -> map to one workstation execution target
  -> execute through the existing delivery/mutation-safety contract
  -> keep every retry/recovery on that same target
```

Everything else in the multi-device product builds on this invariant.
