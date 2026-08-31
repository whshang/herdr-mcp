# Connect a new computer to an existing Herdr Worker

This is the authoritative Agent execution contract for **connecting a new computer to an existing herdr-mcp Worker/Connector**. It is **not** a fresh Worker deployment.

> **macOS only in v0.4.3.** Secure new-device pairing requires the macOS Keychain credential backend. On Linux/Windows the `worker pair` / `worker connect` path is **unavailable and fails closed**; the runtime itself remains supported on those platforms.

## Before you start

- This path requires **v0.4.3+**. Check the latest stable Release version/capability. If stable is still `<0.4.3` or the installed CLI does not expose `herdr-mcp worker pair` / `herdr-mcp worker connect`, **stop and report the version/capability blocker**. Do not install a prerelease/source build unless the user explicitly asked to test preview/source.
- Install the latest stable PROD herdr-mcp from the **GitHub Release**, not from a repo checkout. Do not treat a source/dev build as a normal install.
- This is **not** a fresh Worker deployment. Do **not** create a new Cloudflare Worker, Durable Object namespace, OAuth app/client, Connector, or copy the legacy global `LINK_SHARED_SECRET`. You join the Worker the user already has.

## How pairing works

1. On the **already-authorized existing macOS computer**, the owner runs:

   ```bash
   herdr-mcp worker pair
   ```

   This creates a short-lived pairing session (default **10 minutes**, single-use) and prints:
   - a **pairing address** (the Worker origin plus a high-entropy pairing id in the URL fragment), and
   - a **6-digit verification code** (formatted `123 456`).

2. On the **new computer**, the Agent runs:

   ```bash
   herdr-mcp worker connect "<pairing-address>" --name "<device-name>"
   ```

   The CLI then **prompts for the 6-digit code** (no-echo TTY, or a single non-echo stdin line when noninteractive). The code is **never** a command-line argument and is **never** echoed or logged.

3. On success, the temporary pairing is exchanged for the existing high-entropy per-device secret. The final device secret is stored **only in the macOS Keychain**; the pairing code/session become immediately unusable. No Cloudflare deployment credential and no legacy `LINK_SHARED_SECRET` is used on the joining device.

## Security rules

- The 6-digit code is the intended short-lived pairing credential. It is single-use, expires in 10 minutes, and is limited to **5 wrong attempts** before the session is permanently locked.
- The pairing id is high-entropy and unguessable; it is carried in the pairing address (URL fragment), not in HTTP access-log paths. The final device secret is never in the pairing address.
- The code must **never** appear in argv, shell history, logs, or transcripts. Do **not** use `echo 123456 | ...` or any shell literal that puts the code into shell history.
- The final device credential belongs in the macOS Keychain. Never print or log it.

## Verification

After a successful connect, verify:

```bash
herdr-mcp status
herdr-mcp doctor
herdr-mcp link status
```

Confirm the resulting immutable `device_id`, Link online/healthy, and successful local binding.

## Uncertain delivery / recovery

- If any mutation reports uncertain delivery, **do not blind retry**; inspect current state first.
- If connect fails after server-side consume, rely on the built-in compensation/revoke behavior (exact remote revoke-self + local Keychain cleanup + prior-config restore) and report the evidence. Do not invent manual secret handling.
- If the pairing code is entered incorrectly 5 times, the session is permanently locked; create a new pairing with `herdr-mcp worker pair`.

## Two-device UAT

Formal two-device GA/UAT has not yet passed. This is expected v0.4.3 behavior pending release/UAT.
