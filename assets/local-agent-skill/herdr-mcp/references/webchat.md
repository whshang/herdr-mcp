# Browser extension and WebChat

Use only the supported Herdr-MCP CLI. Do not inspect browser profiles/cookies, inject page scripts, or start a separate browser automation stack.

For the local-agent WebChat control workflow — capability discovery, create/continue/dispatch/observe, canonical handoff, and mutation safety — read `references/webchat-control.md`. This file covers installing/checking the browser bridge and the exact CLI surface.

## Check the bridge

```sh
herdr-mcp native-host status
herdr-mcp extension standalone status
```

For a managed standalone install:

```sh
herdr-mcp extension standalone install
herdr-mcp native-host install
herdr-mcp native-host use standalone
```

Chrome Web Store installation and browser/account sign-in remain human browser actions when required.

## Discover browser resources

```sh
herdr-mcp webchat endpoints [--limit N]
herdr-mcp webchat resources [--endpoint-ref REF] [--provider PROVIDER] [--kind account|space|session] [--parent-ref REF] [--limit N]
herdr-mcp webchat inspect RESOURCE_REF
```

Use returned refs and `observation_generation`; never synthesize refs. `inspect` reports resource metadata, consent, and the resource kind's own inspect capability; that `actuation_available` value is not a generic mutation preflight. For mutations, use the operation's returned delivery state. On `resource_unavailable`, refresh/re-observe the relevant WebChat browser surface before a later retry; never change the idempotency key merely to probe liveness.

## Create a session

```sh
herdr-mcp webchat create \
  --endpoint-ref ENDPOINT_REF \
  --provider chatgpt \
  --account-ref ACCOUNT_REF \
  --display-label LABEL \
  --message MESSAGE \
  --expected-generation N \
  --idempotency-key KEY \
  [--space-ref SPACE_REF] \
  [--work-chain-id WORK_CHAIN_ID]
```

Use one stable idempotency key for one intended mutation. A timeout/uncertain response is not permission to submit the mutation again with a new key.

## Continue an existing session

```sh
herdr-mcp webchat send \
  --session-ref SESSION_REF \
  --message MESSAGE \
  --expected-generation N \
  --idempotency-key KEY \
  [--work-chain-id WORK_CHAIN_ID]

herdr-mcp webchat dispatch-status DISPATCH_ID
```

## Archive

```sh
herdr-mcp webchat archive \
  --session-ref SESSION_REF \
  --expected-generation N \
  --idempotency-key KEY
```

Archive sessions created/owned by the current task after their result/evidence has been captured. Do not archive a user session or another task's session merely because it looks idle.

Browser mutations remain gated by local extension consent, observed capability generation, account-scoped serialization, and Herdr-MCP delivery/idempotency rules. The CLI acquires the local trusted grant internally; it never prints the runtime bearer or browser secrets.
