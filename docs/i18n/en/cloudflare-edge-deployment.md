# Cloudflare Edge

Herdr's Cloudflare Edge does not require users to own a domain.

The recommended deployment is split into two levels:

1. **Default / out of the box: `workers.dev`** — any Cloudflare Workers user can deploy directly without buying or hosting a domain.
2. **Optional / long-lived stable entry: Custom Domain** — when you already have a domain, binding a stable subdomain is recommended, but it is not a prerequisite for running Herdr.

Cloudflare itself defines Custom Domain as the "the Worker itself is the origin" scenario; the Herdr Edge is exactly that architecture. `workers.dev` is ideal for first installs, development, personal use, and standalone verification.

## Architecture

### Default: no custom domain needed

```text
ChatGPT
   |
https://herdr-edge.<account>.workers.dev/mcp
   |
Cloudflare Worker + Durable Object
   ^
authenticated WSS
   |
herdr-link
   |
local Herdr runtime
```

This mode needs none of:

- your own domain;
- DNS records;
- Cloudflare Tunnel;
- a public VPS;
- inbound port mapping.

`herdr-link` actively establishes the WSS connection to Cloudflare; the local machine stays unreachable directly from the public internet.

### Optional: stable Custom Domain

```text
ChatGPT
   |
https://herdr.example.com/mcp
   |
Cloudflare Worker Custom Domain
   |
Cloudflare Worker + Durable Object
   ^
WSS
   |
herdr-link
```

The value of a Custom Domain is **stable naming and ownership**, not a technical requirement of Herdr. Even if the Edge implementation later moves from Cloudflare to another platform, users keep the same MCP/OAuth URL.

## Default deployment: `workers.dev`

Start from the generic template:

```bash
cp edge/cloudflare/wrangler.user.example.toml edge/cloudflare/wrangler.user.toml
```

`wrangler.user.toml` is gitignored, so personal Worker names, workstation IDs, or OAuth issuers are never accidentally committed.

Keep the Worker config as:

```toml
name = "herdr-edge"
main = "src/index.ts"
workers_dev = true
routes = []
```

Deploy:

```bash
cd edge/cloudflare
npx wrangler deploy
```

After deployment Cloudflare provides:

```text
https://<worker-name>.<account-subdomain>.workers.dev
```

The Herdr MCP URL is therefore:

```text
https://<worker-name>.<account-subdomain>.workers.dev/mcp
```

Without a custom domain, the OAuth issuer should also use the same stable `workers.dev` origin, to avoid splitting the MCP endpoint from the OAuth identity:

```toml
[vars]
OAUTH_ISSUER = "https://<worker-name>.<account-subdomain>.workers.dev"
```

> A Worker name or Cloudflare account-subdomain change alters the URL. Once ChatGPT is already connected, treat this origin as a stable identity and do not rename it casually.

## GitHub Actions automatic production Edge deployment

The repo ships:

```text
.github/workflows/cloudflare-edge.yml
```

When Edge / Relay / package deployment surface changes on `main`, the workflow:

1. `npm ci`;
2. runs the full Edge/frozen-contract gate;
3. runs the root regression tests;
4. enters the GitHub `production` Environment when gates are green;
5. deploys `wrangler.prod.toml` with `cloudflare/wrangler-action@v4` + Wrangler major 4;
6. verifies the independent `workers.dev/health` endpoint post-deploy.

Environment secrets:

```text
CLOUDFLARE_ACCOUNT_ID
CLOUDFLARE_API_TOKEN
```

Automatic deployment does **not** modify the Custom Domain, DNS, Tunnel, or OAuth issuer. The production domain keeps pointing at the same Worker service, so ordinary code releases do not require reconnecting the ChatGPT Connector.

The least-privilege target is `Workers Scripts Write`. The current project GitHub Environment temporarily reuses the existing Herdr cutover token (`Workers Scripts Write + Workers Routes Write`, without DNS/Tunnel/Admin permissions); it deploys fine but carries one more permission (Routes Write) than ideal; switch to scripts-only once a bootstrap credential able to mint tokens is available.

The docs site and the Edge are two independent workflows: a Pages failure does not block the Worker, and Worker deploys do not carry Pages credentials. See [`automation.md`](automation.md).

## Optional deployment: Custom Domain

With your own Cloudflare zone, you can bind e.g.:

```text
herdr.example.com
```

to an already-verified Worker.

Wrangler supports:

```toml
[[routes]]
pattern = "herdr.example.com"
custom_domain = true
```

But Herdr recommends separating **code deployment** from **production domain switchover**:

1. always first deploy and verify the Worker on `workers.dev`;
2. confirm `/health`, WSS Link, MCP tools/list, and OAuth all work;
3. only then bind the Custom Domain.

The repo provides a dedicated controller:

```bash
bin/herdr-cloudflare-domain preflight
bin/herdr-cloudflare-domain status
bin/herdr-cloudflare-domain attach
bin/herdr-cloudflare-domain watch
bin/herdr-cloudflare-domain detach
```

It calls the Cloudflare Workers Domains API and does **not** delete or modify DNS, and does **not** stop the Tunnel.

If you are migrating from an old `CNAME -> Cloudflare Tunnel` setup rather than a new install, use the transactional migrator:

```bash
bin/herdr-custom-domain-cutover preflight
bin/herdr-custom-domain-cutover run
```

`run` should execute exactly once. It treats the following steps as one transaction:

1. exact comparison of the old CNAME against local rollback evidence;
2. confirms the production Worker candidate, OAuth identity, and old Tunnel are all healthy;
3. deletes the single conflicting CNAME;
4. attaches the Worker Custom Domain;
5. verifies health, workstation, epoch/hash, the **18-tool epoch-2 catalog including `herdr_skill`**, OAuth/MCP identity, and one read-only `herdr_inspect`;
6. on any step failure, automatically detaches the Custom Domain and restores the original CNAME;
7. for the "server committed but the response was lost" cases of DNS DELETE/POST and Custom Domain PUT/DELETE, re-reads real state instead of blind retries.

Transaction state is written to the user's local `~/.config/herdr-mcp/`, mode `0600`, never into Git.

Corresponding environment:

```text
CLOUDFLARE_API_TOKEN
CLOUDFLARE_ACCOUNT_ID
HERDR_CUTOVER_ZONE
HERDR_CUTOVER_ZONE_ID
HERDR_CUSTOM_DOMAIN
HERDR_CUTOVER_WORKER
HERDR_CUTOVER_PROD_EDGE
HERDR_CUTOVER_WORKSTATION
```

On macOS, the production smoke-test bearer defaults to the Keychain service:

```text
herdr-edge-prod-mcp-bearer
```

so the bearer never has to be written into the repo or the command line.

## Migrating from Tunnel/CNAME to Custom Domain

This is the only scenario needing special handling.

Cloudflare does not allow creating a Worker Custom Domain on the same hostname as an **existing CNAME**. So if the old architecture is:

```text
herdr.example.com -> CNAME -> Cloudflare Tunnel
```

you cannot just overwrite it.

The safe migration order:

1. the production Worker on `workers.dev` fully passes preflight;
2. record the old DNS CNAME's full value and proxied status as rollback evidence;
3. keep the old Tunnel process online;
4. delete the conflicting CNAME;
5. immediately bind the same hostname to the verified Worker via the Workers Domains API;
6. verify the Custom Domain:
   - `/health`;
   - workstation online;
   - epoch-2 `tools/list` with 18 tools including `herdr_skill`;
   - OAuth discovery / token;
   - one real MCP tool call;
7. once observation passes, the old Tunnel exits service;
8. if step 5/6 fails: detach the Custom Domain and restore the old CNAME recorded in step 2; the old Tunnel stayed online the whole time, so it can immediately take traffic again.

Do not shut down the Tunnel first, and never delete the CNAME without DNS rollback evidence.

### One-shot DNS credential

Normal Herdr Edge credentials do not need DNS Edit. When migrating an old CNAME, create a dedicated, **one-shot, target-zone-only `DNS Write` token**, kept in:

```text
~/.config/herdr-mcp/cloudflare-dns-cutover.env
```

The repo provides a dedicated one-shot tool, so DNS permissions never mix into the long-lived Edge token:

```bash
# first use a bootstrap credential with Account API Token management permission
export CLOUDFLARE_API_TOKEN='<bootstrap token>'

# create; DNS Write for the example.com zone only
bin/herdr-cloudflare-dns-token --zone example.com

# non-echoing verification
bin/herdr-cloudflare-dns-token --verify-only

# after the rollback observation window, revoke and delete the local credential file
bin/herdr-cloudflare-dns-token --revoke
```

The script dynamically resolves the Account, Zone, and `DNS Write` permission group; it does not hard-code user account IDs. The created token is never printed to the terminal; the local file is force-chmod `0600`.

It exists only for the migration and rollback observation window; revoke it after the Custom Domain is stable and the old Tunnel has formally exited. Do not add DNS permissions to the long-lived Herdr token for this migration.

## Why the open-source project does not require a Custom Domain

Requiring a domain would add barriers unrelated to Herdr's core capability: domain purchase, Cloudflare zone onboarding, DNS and certificate management.

So the project convention is:

| Deployment mode | Support level | Fits |
| --- | --- | --- |
| `workers.dev` | **default, fully supported** | first installs, personal use, development, testing, users without a domain |
| Custom Domain | **recommended, optional** | long-lived stable entry, team/production environments, fixed OAuth/MCP identity |
| Cloudflare Tunnel straight to local MCP | **legacy / migration compatibility** | upgrading from an old version; no longer the default architecture for new installs |

## Security boundary

Whatever domain mode you use, keep these boundaries:

- the Cloudflare Edge is the public entry;
- `herdr-link` only establishes outbound WSS;
- the local Herdr runtime does not expose a public port directly;
- Link secrets, OAuth signing material, and MCP bearers never enter Git;
- tokens use least privilege; see [`cloudflare-edge-token.md`](cloudflare-edge-token.md);
- Custom Domain switchover and Worker code deployment are two independent operations that can be rolled back separately.

## Related scripts

```text
bin/herdr-cloudflare-token       # create/verify least-privilege Cloudflare token
bin/herdr-cloudflare-domain      # Custom Domain attach/watch/detach
bin/herdr-custom-domain-cutover  # transactional CNAME/Tunnel -> Custom Domain migration
bin/herdr-link                   # workstation -> Edge WSS sidecar
```

New installs prefer `workers.dev`; users with an existing stable domain can choose Custom Domain. Do not treat the custom-domain examples in the docs as an install prerequisite.