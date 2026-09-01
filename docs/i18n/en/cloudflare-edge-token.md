# Cloudflare credentials

*Least privilege, temporary bootstrap, and verifiable state.*

Deploying herdr-mcp Edge requires Cloudflare API permissions, but long-lived operation should not depend on an account-wide administrator credential.

This page explains the project's least-privilege Cloudflare Account API Token workflow.

## Keep credential roles separate

Two credential roles may appear during deployment:

1. **bootstrap credential** — an existing higher-privilege credential used temporarily to create the target token;
2. **deployment credential** — the least-privilege token used by later herdr-mcp deployment/cutover work.

The goal is to remove the bootstrap credential from the normal workflow as soon as possible.

## Target permissions

The project helper creates a token scoped to:

- **Workers Routes Write** for the target zone;
- **Workers Scripts Write** for the corresponding account;
- **Workers R2 Storage Write** for the corresponding account, so Worker setup can create and verify the private artifact bucket before deploy.

It does not request broad account administrator access by default. Cloudflare may label the R2 capability as **Workers R2 Storage:Edit** in parts of the UI; the API permission-group name used by the helper is **Workers R2 Storage Write**.

A pure `workers.dev` deployment may not need a zone route for every operation. Grant only the permissions required by the deployment path you actually use.

## Helper command

```text
bin/herdr-cloudflare-token
```

Inspect options:

```bash
bin/herdr-cloudflare-token --help
```

Common modes:

```bash
# Resolve identity and validate bootstrap permissions without creating a token
bin/herdr-cloudflare-token --zone example.com --dry-run

# Create and save the least-privilege credential
bin/herdr-cloudflare-token --zone example.com

# Verify the already-saved credential
bin/herdr-cloudflare-token --zone example.com --verify-only

# Explicitly replace an existing saved credential
bin/herdr-cloudflare-token --zone example.com --rotate
```

Provide the bootstrap credential through the process environment:

```bash
export CLOUDFLARE_API_TOKEN='<temporary-bootstrap-token>'
# or CF_API_TOKEN
```

Do not copy the real value into repository files, commits, screenshots or chat transcripts.

## Local credential state

The helper defaults to:

```text
~/.config/herdr-mcp/cloudflare-cutover.env
```

The file is written with restricted local permissions (**mode `0600`**) and the token value is not printed to stdout.

It also records account/zone identity so later verification can test the intended Workers Scripts and Routes access.

This file is local credential state, not project configuration. It must not be committed.

## Why dry-run first

Token creation is a mutation. On a new Cloudflare account or zone, start with:

```bash
bin/herdr-cloudflare-token --zone <zone> --dry-run
```

This can catch missing/ambiguous zone identity, insufficient bootstrap permissions and permission-group problems before generating a new credential.

## Why rotation is explicit

If a local credential already exists, the helper does not silently replace it. `--rotate` is required.

Credential rotation can affect active deployments, CI or other scripts still using the previous token, so replacement requires explicit intent.

## What verification means

`--verify-only` checks more than the presence of a token string. The saved credential should be active and usable against the expected Cloudflare account/zone APIs.

A good verification confirms:

- token active state;
- correct account identity;
- Workers Scripts access;
- Workers Routes access for the target zone where required;
- Workers R2 Storage access for private artifact-bucket provisioning.

When deployment fails, distinguish credential failure from Worker/DO configuration failure.

## ChatGPT never needs this credential

These are separate layers:

```text
Cloudflare API token
  purpose: deploy and maintain Edge

ChatGPT OAuth token
  purpose: ChatGPT accesses the deployed MCP Edge

HERDR_MCP_TOKEN
  purpose: local curl / Cursor / legacy local compatibility
```

Do not copy one into another layer. In particular, Cloudflare API tokens and `HERDR_MCP_TOKEN` do not belong in the ChatGPT Connector UI.

## Credential hygiene

Recommended practice:

- pass bootstrap credentials through temporary process environment;
- store the least-privilege credential only in a restricted local file or proper Secret Store;
- do not put secrets in `wrangler.toml`, README files, examples or Git;
- do not echo secret values into terminal logs;
- log readiness/status, not credential contents;
- investigate architecture/configuration before expanding permissions;
- verify the new token after rotation before removing the old one.

## Relationship to Edge deployment

For a new installation, validate Worker, workstation link, OAuth and MCP on `workers.dev` first, then add Custom Domain/routes as a separate step.

See [Cloudflare Edge deployment](cloudflare-edge-deployment.md) for architecture and deployment, and [Installation](install.md) for the shortest end-to-end path.
