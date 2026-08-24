# Automation

Audience: maintainers operating CI/CD, Pages, production Edge deployment, and local runtime release automation. End users do not need this page for first setup.

herdr-mcp has three intentionally separate automation planes. They share source control, but they do not share credentials or failure domains.

## 1. GitHub Pages

Workflow: `.github/workflows/pages.yml`

Published site:

```text
https://whshang.github.io/herdr-mcp/
```

The Pages artifact contains:

- `site/` — static product/install page;
- rendered HTML for every tracked `docs/*.md` page (excluding `docs/_wip/`);
- `herdr-mcp-SKILL.md` — public remote-planner policy copied from `assets/herdr-mcp-SKILL.md`;
- `release.json` — current package version + Git commit + docs/skill locations.

The repository is public, and Pages uses the repository's native GitHub Pages deployment. `npm run build:site` is the single build path used by both CI and the Pages workflow, so a documentation change cannot bypass the same static-site build used for publication.

This makes Pages both the human-facing documentation site and a credential-free update source for `herdr_skill`.

`herdr_skill` uses the Pages skill URL by default, caches it, and falls back to the release-bundled `assets/herdr-mcp-SKILL.md` if Pages/network is unavailable. Set `HERDR_SKILL_NETWORK=0` for fully offline behavior or `HERDR_MCP_SKILL_URL` to override the policy endpoint.

## 2. CI

Workflow: `.github/workflows/ci.yml`

Every push to `main` and every pull request runs:

1. `npm ci`;
2. TypeScript build;
3. documentation site build (`npm run build:site`);
4. root test suite;
5. Edge/frozen-contract suite;
6. browser-extension smoke;
7. shell syntax checks;
8. npm package dry-run;
9. `git diff --check`.

The root runtime version may move independently from the ChatGPT public contract epoch. Production currently freezes epoch 2 at 18 tools; epoch-1 compatibility tests remain only to prove the historical 17-tool rollback/old-session ABI is still reproducible.

## 3. Cloudflare Edge production deployment

Workflow: `.github/workflows/cloudflare-edge.yml`

The workflow runs only for `main` changes that affect the Edge/Relay/package deployment surface, or by manual dispatch. It first runs the Edge/contract and root regression gates, then deploys `edge/cloudflare/wrangler.prod.toml` with `cloudflare/wrangler-action@v4` and Wrangler major 4.

The deploy job uses the GitHub Environment:

```text
production
```

Required Environment secrets:

```text
CLOUDFLARE_ACCOUNT_ID
CLOUDFLARE_API_TOKEN
```

The least-privilege target is **Workers Scripts Write only** for the target account. The currently provisioned GitHub Environment secret reuses the existing Herdr cutover credential, which has **Workers Scripts Write + Workers Routes Write** and still has no DNS Write, Tunnel Edit or Account Admin. This is sufficient for deployment but should be replaced by a scripts-only token when a bootstrap credential capable of minting API tokens is available. Custom Domain ownership/routing is intentionally not mutated by the deploy workflow.

After deployment the workflow checks the independent workers.dev health endpoint. The existing Custom Domain continues to target the same Worker service.

## 4. Local runtime self-update

CLI: `herdr-self-update`

The runtime release plane is intentionally separate from Cloudflare deployment. Updating local herdr-mcp must not restart the public Edge or persistent `herdr-link`.

Typical remote-planner flow:

```bash
herdr-self-update status
herdr-self-update check
herdr-self-update apply --source remote --ref main
```

For testing an uncommitted development tree:

```bash
herdr-self-update apply --source working-tree
```

`apply` starts a detached supervisor and returns before the current MCP runtime is restarted. The worker records structured progress under `~/.config/herdr-mcp/`, builds/tests an isolated release, starts a loopback candidate, uses the persistent generation manager to validate and activate it, reloads the stable 8772 runtime from the new release, promotes the new stable generation and removes the temporary candidate.

The updater inherits the current contract profile. It **does not** automatically change the ChatGPT contract epoch, DNS, OAuth issuer, Custom Domain or Edge deployment.

After an update, verify from the same remote Connector:

- `herdr_inspect` reports the new runtime version;
- generation status reports the new stable generation;
- Edge `/status/<workstation>` converges to that version/generation;
- the public contract epoch/hash remain unchanged unless an explicit contract migration was intended.

## 5. `herdr_skill` responsibility

`herdr_skill` is not merely the official Herdr usage tutorial. It composes three layers:

1. **herdr-mcp project policy** — direct edit/tool order, agent dispatch preferences, mutation/idempotency rules, browser boundary and self-maintenance procedure;
2. **live runtime context** — running version, contract profile, generation/self-update state;
3. **release-matched native Herdr reference** — `herdr --skill`, clearly scoped as pane-local reference so its `HERDR_ENV=1` rule does not incorrectly stop a remote web planner.

Project policy has precedence for ChatGPT/Web usage. `herdr_methods` remains the live authority for installed native socket method names and schemas.
