# Multi-device control

*Use one Herdr Worker and one ChatGPT connection to control multiple enrolled computers.*

A Herdr fleet has one public Worker/Connector and multiple independently identified computers behind it. ChatGPT can discover the fleet, choose a device for a task, and keep follow-up operations attached to that device. New computers join the existing Worker through short-lived pairing; they do not deploy another Worker or receive a shared global secret.

> Secure new-device pairing currently uses the macOS Keychain credential backend, so the pairing workflow is currently macOS-only.

## See the fleet from ChatGPT

Use `herdr_devices` to list the devices known to the Worker. The result includes stable device identity plus current authorization, connection, scheduling, and health information.

A useful prompt is:

```text
List my Herdr devices and show which are online. Use macbook-main for the backend task and macbook-lab for the independent test task. Keep their working trees isolated and verify both results before reporting completion.
```

Routing is intentionally conservative:

- an explicitly named device is used for that operation;
- follow-up references and retries keep their original device identity;
- when only one device is routable, Herdr can select it automatically;
- when several devices are valid candidates for a mutation and no target is specified, Herdr returns `device_ambiguous` instead of guessing.

Each enrolled computer has its own credential and immutable `device_id`. Device names are human-friendly selectors; the underlying identity remains stable.

## Add a new computer

### 1. Preferred: create the pairing directly in ChatGPT

In an already-authorized Herdr conversation, simply ask:

```text
Generate a Herdr pairing link for my new computer, valid for 10 minutes.
```

ChatGPT can create the pairing directly at Edge; no enrolled workstation has to be online. The response should show together:

- the pairing address containing a high-entropy pairing id;
- the single-use 6-digit verification code;
- the exact expiry time; and
- the copyable `herdr-mcp worker connect "<pairing-address>"` command.

The normal maximum TTL is 600 seconds. Use the pairing immediately instead of treating it as a durable invitation.

CLI fallback: on any authorized macOS computer in the fleet, `herdr-mcp worker pair` creates the same short-lived pairing and now prints the exact UTC expiry as well as the relative validity window.

### 2. Connect the new computer

On the new computer, the Agent runs:

```bash
herdr-mcp worker connect "<pairing-address>"
```

The CLI then prompts for the 6-digit code using a no-echo input. The code is intentionally not accepted as a normal command-line argument.

By default, the joining computer registers its macOS **Computer Name** as the device display name. Use `--name "<device-name>"` only when the user explicitly wants a different initial name. An owner-supplied `worker pair --name ...` is also an explicit override and takes precedence.

After the pairing is consumed, `worker connect` installs/starts the local `herdr-mcp` service and ensures the enrolled Rust production Link is created and loaded. The command reports success only after the local service is healthy and `link-prod` is owned by the managed runtime with the new device identity; a failure triggers the existing revoke/Keychain/config compensation path.

For an Agent-assisted setup, paste this sentence on the new computer:

```text
Connect this computer to my existing Herdr fleet by following https://github.com/whshang/herdr-mcp/blob/main/docs/i18n/en/existing-worker-connect.md; use this pairing address: <pairing-address>, ask me for the 6-digit verification code only when the CLI prompts for it, then verify this device appears online in the same Worker.
```

### 3. Verify the new device

After the connection succeeds:

```bash
herdr-mcp status
herdr-mcp doctor
herdr-mcp link status
```

Then ask ChatGPT to call `herdr_devices` and confirm the new device is online under the same Worker.

To explicitly rename the current enrolled computer later, run:

```bash
herdr-mcp worker rename "<new-device-name>"
```

`herdr-mcp device rename ...` is an equivalent alias. Rename changes only the human-facing display name; the immutable `device_id`, workstation identity, credential, authorization and scheduling state stay unchanged. Link reconnects do not overwrite an explicit rename. The default/legacy workstation likewise records its local Computer Name when it is first registered.

To permanently remove authorization from another enrolled device, first get its immutable `device_id` from `herdr_devices`, then run this on the owner workstation:

```bash
herdr-mcp worker revoke "<device-id>" --confirm
```

Revocation is permanent for that device identity and credential: the live Link is disconnected, the old credential can never reconnect, and the revoked tombstone is retained internally to prevent resurrection. Revoked tombstones are hidden from normal fleet/device lists. To add that computer again later, create a new pairing and enroll it as a new device identity.

## What pairing changes

The short-lived pairing is exchanged for a new per-device credential. The final credential is stored in macOS Keychain, and the Worker stores only the verifier needed to authenticate that device. The pairing session becomes unusable after successful consumption.

The joining computer does **not** need:

- Cloudflare deployment credentials;
- a new Worker or Durable Object deployment;
- a new ChatGPT Connector/OAuth client; or
- the legacy global `LINK_SHARED_SECRET`.

## Pairing security

- The 6-digit code is single-use and short-lived.
- Five wrong code attempts permanently lock that pairing session; create a new one instead of retrying indefinitely.
- The pairing id is high-entropy and stays in the URL fragment so it is not placed in normal HTTP access-log paths.
- The OAuth-authorized owner conversation may display the one-time code when the user explicitly creates a pairing there. Outside that narrow flow, never persist the code in argv, shell history, Git, ordinary logs, copied transcripts, or unattended automation.
- Never print or copy the final per-device credential; it belongs in the OS credential store.

## Recovery

If a mutation reports uncertain delivery, inspect current state before retrying. Do not blindly repeat an operation whose delivery may already have happened.

If connection fails after the server has consumed the pairing, rely on the built-in compensation/revoke behavior and inspect the resulting state. Create a fresh pairing only after the previous attempt is known to be unusable.
