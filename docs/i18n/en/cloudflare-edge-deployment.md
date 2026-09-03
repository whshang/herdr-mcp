# Cloudflare Edge

*A stable public entry for a private workstation.*

ChatGPT is on the public Internet while a Herdr workstation is usually behind NAT, firewalls or a corporate network. herdr-mcp does not require opening an inbound port on the development machine. The workstation creates an outbound authenticated connection to Cloudflare Edge.

```text
ChatGPT
   │ HTTPS / OAuth / MCP
   ▼
Cloudflare Worker + Durable Object
   ▲
   │ authenticated WSS
herdr-link
   │
   ▼
local herdr-mcp runtime
   │
   ▼
Herdr / Git / shell
```

This page explains the deployment model, the zero-domain `workers.dev` bootstrap path, the recommended Custom Domain production path, and how legacy Tunnel/CNAME deployments should migrate safely. The open-source setup **does not require users to own a domain**: `workers.dev` remains fully supported, but when the selected Cloudflare Account already has an active zone, finalize a dedicated Custom Domain before registering OAuth/MCP clients.

## Three rules to remember

1. **Bootstrap new installations on `workers.dev`; prefer a Custom Domain before client registration when an active zone is available.** No domain is required, and Cloudflare manages the Custom Domain DNS record/certificate automatically.
2. **The workstation only connects outbound.** The public Internet does not reach `127.0.0.1:8772` directly.
3. **Treat the public origin as an identity.** Connector URL, OAuth issuer and MCP resource should remain stable once validated.

## What Edge owns

The Cloudflare layer provides:

- stable HTTPS MCP endpoint;
- OAuth discovery/authorization/token flow;
- workstation identity and routing;
- persistent WSS link management;
- runtime online/offline and generation/version state;
- MCP request/response relay;
- optional short-lived private R2 generic artifact relay (`/artifacts`, Worker-only bucket).

Edge does not store your Git repositories or replace Herdr. Code, shell commands and agents still run on the workstation. The R2 bucket is an ephemeral generic artifact relay, not an asset library.

## Bootstrap deployment: workers.dev

Copy the user template:

```bash
cp edge/cloudflare/wrangler.user.example.toml edge/cloudflare/wrangler.user.toml
```

The user file is deployment-local and should not become a repository source of workstation identity or local deployment values.

### Generate a valid Worker name

Do not copy a machine hostname verbatim. Hostnames commonly contain dots or other characters unsuitable for a Worker DNS label.

```bash
WORKER_NAME="$(node scripts/cloudflare-worker-name.mjs "$(hostname)")"
printf '%s\n' "$WORKER_NAME"
```

A `workers.dev` Worker name is a DNS label. A Custom Domain is a full hostname; they follow different naming rules.

### Public origin

After deployment, Cloudflare provides an origin similar to:

```text
https://<worker>.<account-subdomain>.workers.dev
```

MCP endpoint:

```text
https://<worker>.<account-subdomain>.workers.dev/mcp
```

During bootstrap this hostname proves that the Worker code is healthy. Do not register the final ChatGPT Connector yet if you intend to use a Custom Domain; the OAuth issuer / `HERDR_MCP_BASE_URL` must match the canonical origin selected before clients attach.

### Deploy

Deploy the core Worker directly. The ordinary user template leaves R2 disabled, so the core path does not require an R2 subscription or payment method.

```bash
cd edge/cloudflare
npx wrangler deploy --config wrangler.user.toml
```

The private R2 artifact relay is optional. If explicitly enabled, add the `ARTIFACT_BUCKET` binding, provision it with `node provision-r2.mjs --config wrangler.user.toml`, and keep the bucket Worker-only with no public r2.dev hostname.

A successful Worker deployment proves only that public code exists. Before client registration, select the final public origin; the workstation link still needs to be online.

## Finalize the public origin before OAuth/MCP clients attach

If the selected Cloudflare Account has an active zone, a dedicated Custom Domain such as `herdr-mcp.example.com` is the recommended production identity. Cloudflare recommends production Workers on a route or Custom Domain rather than relying on `workers.dev`; because the Herdr Worker is the origin for this hostname, use a **Custom Domain**, not a Worker Route in front of another origin.

For Wrangler:

```toml
[[routes]]
pattern = "herdr-mcp.example.com"
custom_domain = true
```

Set `OAUTH_ISSUER=https://herdr-mcp.example.com`, redeploy, and verify `/health`, unauthenticated `/mcp`, and OAuth discovery on the Custom Domain before creating the ChatGPT Connector. Cloudflare creates the required DNS record and certificate on your behalf. The hostname must belong to an active Cloudflare zone and must not conflict with an existing CNAME or incompatible Worker/DNS use.

If no suitable zone exists or the user chooses not to use one, keep `workers.dev` as the canonical public origin. That is a supported configuration, not an installation failure.

## Workstation Link

`herdr-link` creates the authenticated outbound WSS connection:

```text
workstation ── outbound WSS ──► Edge
```

It carries workstation identity, receives requests for that workstation, routes them to the active local runtime generation, and reports runtime generation/version by heartbeat.

This separation lets the public Connector stay stable while the local runtime restarts or switches A/B generations.

If OAuth and public `/health` work but tool calls report `workstation offline`, investigate the link instead of reinstalling the Connector.

On v0.4.3+, Edge first gives a recently connected workstation a short in-memory reconnect grace. If no validated Link returns, the MCP error exposes `retryable`, `delivery_state`, `retry_after_ms` and a bounded read-only recovery policy so the Agent does not have to guess whether replay is safe. The workstation Link has its own reconnect/backoff and prolonged-offline recycle path; this recovery does not use browser-extension state and does not add request-led Durable Object writes/alarms. See [Troubleshooting](troubleshooting.md) for the exact replay rules.

## First validation sequence

Validate layer by layer:

```text
1. local runtime
2. herdr-link
3. Edge health
4. OAuth metadata/token
5. public MCP initialize/tools/list
6. real herdr_inspect
7. new ChatGPT conversation
```

This distinguishes Edge deployment failures from workstation reachability failures quickly. See [Troubleshooting](troubleshooting.md).

## Why direct Cloudflare Tunnel is no longer the default

A direct tunnel architecture is simple:

```text
ChatGPT → Tunnel → local MCP
```

but it couples the public endpoint too tightly to one local process. Runtime restarts affect the public path, OAuth identity and machine lifecycle become intertwined, and multi-workstation routing or runtime A/B become awkward.

The preferred architecture is:

```text
ChatGPT → stable Edge ← persistent link ← workstation
```

Direct Tunnel remains a legacy migration path, not the new-installation architecture.

## When to use a Custom Domain

Use a Custom Domain by default when the account already has a suitable active Cloudflare zone and the hostname can be dedicated to Herdr. A hostname such as:

```text
https://herdr.example.com
```

gives long-lived naming, organization-owned OAuth identity, team governance and future implementation migration without changing the external URL. It can also avoid network paths where `workers.dev` is filtered while the user's own Cloudflare hostname remains reachable.

It is still not a technical prerequisite for Herdr. Do not make domain ownership a prerequisite for first install.

## Custom Domain operations

The repository keeps domain operations separate from Worker code deployment:

```bash
bin/herdr-cloudflare-domain preflight
bin/herdr-cloudflare-domain status
bin/herdr-cloudflare-domain attach
bin/herdr-cloudflare-domain watch
bin/herdr-cloudflare-domain detach
```

Recommended sequence for a first install:

```text
bootstrap + validate workers.dev
      ↓
discover active zone / recommend dedicated hostname
      ↓
attach
      ↓
set OAuth issuer + validate health / OAuth / MCP
      ↓
register Connector + validate workstation
```

Deploying new Worker code and changing the production hostname should remain independently reversible operations.

## Migrating an old CNAME / Tunnel deployment

Only existing legacy installations need this path.

Old shape:

```text
herdr.example.com
  ↓ CNAME
Cloudflare Tunnel
  ↓
local runtime
```

If the hostname already has a conflicting DNS record, a Worker Custom Domain cannot simply replace it without a cutover.

Safe migration principles:

1. fully validate the new Worker on an independent `workers.dev` origin;
2. record old DNS/Tunnel rollback evidence;
3. keep the old Tunnel online during cutover;
4. remove only the conflicting record;
5. attach the Worker Custom Domain;
6. validate public health, workstation, OAuth, current MCP contract and a real read-only tool call;
7. retire the old Tunnel only after the new path is stable;
8. restore the previous entry if any validation fails.

Transactional helper:

```bash
bin/herdr-custom-domain-cutover preflight
bin/herdr-custom-domain-cutover run
```

As with all remote mutations, uncertain DNS/domain delivery is resolved by reading actual Cloudflare state, not blindly repeating requests.

Legacy CNAME cutover is the only path that should need DNS mutation. Use a **one-shot, target-zone-only `DNS Write` token** instead of expanding the long-lived Edge deployment credential. After the rollback observation window closes, revoke it and remove its local state:

```bash
bin/herdr-cloudflare-dns-token --verify-only
bin/herdr-cloudflare-dns-token --revoke
```

## Cloudflare API credentials

Deployment credentials are unrelated to ChatGPT OAuth.

```bash
bin/herdr-cloudflare-token --zone example.com --dry-run
bin/herdr-cloudflare-token --zone example.com
bin/herdr-cloudflare-token --zone example.com --verify-only
```

Use least privilege and keep DNS Write as a separate, short-lived credential when legacy cutover genuinely requires it. See [Cloudflare Edge credentials](cloudflare-edge-token.md).

## GitHub Actions deployment

The repository production Edge workflow builds/tests the relevant Edge/contract surface, passes the production Environment gate, deploys with Wrangler and performs a post-deploy health check.

CI credentials belong in GitHub Environment/Secrets, not repository files.

Normal Worker code deployment should not modify:

- Custom Domain;
- OAuth issuer;
- workstation identity;
- DNS;
- ChatGPT Connector URL.

Keeping these boundaries separate makes code deployment and production-entry changes independently reversible.

## Edge and Runtime A/B are separate release planes

```text
Public plane
Cloudflare Edge / OAuth / Connector URL

Local plane
herdr-link → runtime generation A/B
```

Most runtime implementation fixes should ship on the local generation plane without changing public Edge identity. Likewise, an Edge relay/OAuth update should not require restarting Herdr.

See [Runtime A/B](runtime-self-upgrade.md).

## Security boundary

- no public inbound workstation port;
- authenticated workstation WSS;
- ChatGPT uses OAuth, not the local static bearer;
- Cloudflare API credentials and OAuth signing material stay out of Git;
- deployment credentials use least privilege;
- Worker deployment and Domain/DNS mutation are separate operations;
- Edge is a remote control plane while real code and execution remain local.

## Deployment choices

| Situation | Recommendation |
|---|---|
| first install / personal use | `workers.dev` |
| long-lived personal endpoint | `workers.dev` or stable Custom Domain |
| team/production environment | Custom Domain + Environment secrets |
| legacy Tunnel/CNAME | validate Worker in parallel, then transactional cutover |
| local Cursor/curl only | no Cloudflare Edge required |

For the shortest path to a working ChatGPT setup, return to [Installation](install.md). This page is primarily for understanding and operating the public control plane.
