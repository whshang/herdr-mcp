# Automation

*Deploy documentation, Edge, and the local Runtime independently.*

This maintainer page explains **how CI/CD executes**. It does not redefine the release model.

There is one long-lived SSOT for release boundaries: [`docs/release-model.md`](../../release-model.md). That document defines Runtime, Browser Extension, and Contract Compatibility as version/compatibility planes. This page instead describes the everyday **deployment surfaces** exercised by automation:

```text
Documentation publish     Public Edge deploy       Local Runtime activation
GitHub Pages               Cloudflare Worker        runtime generation A/B
       │                          │                         │
Human + agent docs          OAuth / MCP / WSS        fs / Git / shell / Herdr
```

These actions share a repository but should not share credentials, rollback boundaries, or failure domains. Browser Extension distribution and contract migration have their own flows; this page only explains how they interact with CI.

## Why automation must stay decoupled

A routine fix should not casually change OAuth identity, Cloudflare routing, local runtime generation, browser-extension identity, and the ChatGPT tool contract at the same time.

The operating rule is: **trigger only the deployment action required by the task, while keeping the other release/compatibility planes stable.**

Examples:

- documentation fix → publish Pages only;
- Edge relay fix → deploy the Worker only;
- runtime implementation fix → qualify and switch only the local generation;
- public tool-catalog change → use the explicit contract-compatibility migration;
- extension-only UI fix → use the extension distribution path without publishing a Runtime release.

## GitHub Pages

Workflow:

```text
.github/workflows/pages.yml
```

Site:

```text
https://whshang.github.io/herdr-mcp/
```

Build entry:

```bash
npm run build:site
```

The site generator validates the logical document model, locale completeness, navigation and generated pages.

Pages serves both:

```text
Human documentation

Remote planner policy
```

`herdr_skill` can use the published skill source and fall back to the bundled release copy when network access is unavailable. `HERDR_SKILL_NETWORK=0` forces offline behavior.

## CI

Workflow:

```text
.github/workflows/ci.yml
```

CI proves that a commit does not break other planes. Typical gates include:

- dependency install;
- TypeScript build;
- documentation site build;
- runtime tests;
- Edge/frozen-contract tests;
- extension smoke tests;
- shell syntax checks;
- package dry-run;
- `git diff --check`.

The public Edge contract is intentionally more stable than runtime implementation. The current public contract remains **epoch 3 / 19 actions**, introduced in v0.4.3, while the workstation Runtime Execution Contract remains **epoch 2 / 18 tools**. The extra public action, `herdr_devices`, executes at Edge and is never forwarded to a workstation. Historical compatibility tests may exist, but normal runtime changes should not silently change either contract.

### GitLab CI and other unattended MCP callers

Do not put `HERDR_MCP_TOKEN`, `STATIC_MCP_BEARER_SECRET`, or one shared fleet-wide access token into CI. Those credentials do not provide per-pipeline identity or independent revocation.

Provision an **Automation Client** from any enrolled workstation instead:

```bash
herdr-mcp automation create --name "gitlab:group/project:prod"
herdr-mcp automation list
herdr-mcp automation rotate <svc_client_id> --confirm
herdr-mcp automation revoke <svc_client_id> --confirm
```

Use a separate client for each meaningful trust boundary, normally at least per GitLab project and environment. `create` returns the long-lived `client_secret` once; `rotate` returns its replacement once. The Worker stores only the verifier. Put these values in masked/protected GitLab variables:

```text
HERDR_MCP_URL
HERDR_MCP_CLIENT_ID
HERDR_MCP_CLIENT_SECRET
```

At job start, exchange the client credentials at the Worker's `/oauth/token` endpoint with `grant_type=client_credentials`. The result is a short-lived MCP access token with a maximum lifetime of one hour and no refresh token. Minting a token updates bounded inventory metadata (`last_token_issued_at_ms` and `token_issue_count`) instead of writing one Durable Object record per MCP request.

Automation Clients have ordinary MCP authority only. They cannot administer the fleet, pair/revoke devices, approve/revoke Connectors, or create another Automation Client. Revocation is keyed by immutable `client_id`, blocks future token minting, and fences already-issued access tokens at Worker verification time.

An explicitly approved WebChat may list Automation Clients and revoke one through private Edge-local methods. Long-lived client secrets are intentionally not returned by inventory and should not be copied into a chat transcript; create/rotate them from an enrolled workstation CLI and store them directly in the CI secret manager.

## Cloudflare Edge deployment

Workflow:

```text
.github/workflows/cloudflare-edge.yml
```

Edge automation manages the public control plane:

- Worker / Durable Object;
- OAuth;
- MCP relay;
- workstation routing;
- post-deploy health checks.

Deployment secrets belong in GitHub Environment/Secrets.

A normal Worker deployment should not automatically modify:

- Custom Domain;
- DNS;
- legacy Tunnel state;
- OAuth issuer;
- workstation identity;
- local runtime generation.

Domain and DNS mutations are separate operations with separate rollback evidence.

See [Cloudflare Edge deployment](cloudflare-edge-deployment.md) and [Cloudflare Edge credentials](cloudflare-edge-token.md).

## Local runtime automation

Local releases use runtime generations instead of replacing the active process in place:

```text
stable A
  ↓
candidate B
  ↓
health + contract gate
  ↓
activate
  ↓
keep rollback target
```

Common entry points:

```bash
bin/herdr-runtime-generation status
bin/herdr-self-update status
bin/herdr-self-update check
```

`herdr-self-update` automates candidate build, validation, activation and observation. It does not own:

- contract epoch migration;
- Edge deployment;
- OAuth issuer migration;
- DNS / Custom Domain changes.

See [Runtime A/B](runtime-self-upgrade.md).

## Contract epoch migration

A public MCP tool surface change is different from a runtime implementation update.

```text
runtime implementation upgrade
        ≠
public MCP contract migration
```

A contract migration affects ChatGPT tool snapshots and requires explicit evidence across local runtime, Link identity, public Edge and new conversation validation.

## Browser extension release

The extension is the continuity layer. It shares repository versioning but keeps separate trust boundaries.

Validation includes:

- manifest and JavaScript compatibility;
- Native Messaging host;
- workspace binding;
- Auto gates;
- progress/settled behavior;
- recovery/handoff;
- JSON → MCP bridge.

Real browser UAT is still required because Node tests cannot prove page behavior.

## `herdr_skill`

`herdr_skill` combines:

1. herdr-mcp project policy;
2. runtime / contract / generation context;
3. matching Herdr guidance.

It guides Web planner behavior. It does not replace CI, deployment scripts or runtime management.

`herdr_methods` remains the authority for the installed Herdr Socket API schema.

## Release decision table

| Change | Release plane |
|---|---|
| Docs, navigation, tutorials | Pages |
| Worker/OAuth/relay | Edge |
| Local implementation | Runtime A/B |
| Browser continuity | Extension + compatibility validation |
| Tool catalog/schema ABI | Contract epoch migration |
| Custom Domain/DNS | Domain cutover |

## What completion means

A green workflow is not the final proof.

The corresponding runtime evidence is required:

- Pages: generated pages and links work;
- Edge: health + workstation + OAuth/MCP work;
- Runtime: active generation + real tool call + rollback target;
- Extension: real site binding/Auto/recovery smoke;
- Contract: a new conversation receives the expected tool snapshot.

Automation is valuable because it fixes verification and rollback boundaries, not because it removes every human decision.
