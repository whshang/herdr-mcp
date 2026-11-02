# herdr-core (`hc`) OpenRPC probe — spike findings (#767)

Status: spike complete, input for the 1.2 runtime-adapter decision (#768). No product change.
Spike date: 2026-11-02. Environment: macOS 26.6.2, arm64, user session.

| Item | Value | How known |
| --- | --- | --- |
| `hc --version` | `hc 0.1.14` | observed |
| installed binary | `~/.local/bin/hc` → `herdr-core` (Mach-O arm64, 5,029,184 bytes) | observed |
| core process | `herdr-core serve`, uptime ≈ 3d 0h 54m **before** the spike started | observed (`hc server status`, `ps`) |
| OpenRPC document | `openrpc: 1.3.2`, `info.version: 0.1.14`, 32 methods, 34 component schemas | observed (`hc schema`) |
| protocol generation | `1` (from the schema `info.description`) | observed |
| license | Apache-2.0 (schema `info.license`) | observed |

Nothing in this spike started a core, added a machine, attached to an existing terminal, or touched
the Herdr TUI server or the herdr-mcp runtime. The core that served every read was already running
when the spike began. One disposable terminal was created, driven, killed and removed; the terminal
list was empty again at the end (`hc ls --all` → `{"hidden_finished":0,"terminals":[]}`).

Evidence tags used below:

- **[observed]** — executed against the live 0.1.14 core and inspected.
- **[schema]** — from `hc schema` (OpenRPC document) only.
- **[help]** — from `hc --help` / `hc <cmd> --help` / `hc --skill` (the bundled manual) / strings embedded in the installed binary. Not exercised.
- **[not verified]** — no local evidence; recorded as an open question for #768.

---

## 1. Socket, authentication, Windows

### 1.1 Socket location and permissions

**[observed]** The core listens on a Unix domain socket inside a per-session state directory:

```
$HOME/.local/state/herdr-core/default/default.sock      mode srwxr-xr-x  owner <user>:staff
$HOME/.local/state/herdr-core/                          mode drwx------
$HOME/.local/state/herdr-core/default/                  mode drwx------
```

Sibling state in the same session directory (`core.json`, `core.lock`, `core.log`, `peers.json`,
`terminals.json`, `screens/`) is mode `0644` *inside* the `0700` directory. The top-level
`$HOME/.local/state` is `0755`; `herdr-core/` and `default/` are `0700`, so the socket is not
reachable by other users even though the socket node itself is `0755` (connect also needs write on
the socket, which group/other do not have).

**Authentication model: local filesystem ownership only.** There is no token, no handshake secret
and no per-client credential:

- `hello` has four parameters — `generation`, `calls_you`, `core`, `framed` — none of which is a
  secret **[schema]**. `CoreInfo` is `{id, name}` and `name` is the hostname by default **[schema]**.
- The only failure `hello` documents is `unsupported_generation` **[schema]**.
- Peer bookkeeping (`peers.json`) is a set of peer ids, not a credential store **[observed:
  file is a small map keyed by a peer id; values not reproduced here]**.

Anything that can run as this user can drive every terminal in the session. That is a materially
weaker boundary than herdr-mcp's authenticated loopback + outbound Link + device credential model,
and it means an "hc backend" cannot reuse herdr-mcp's authorization layer as-is.

Transport details **[schema]**:

- JSON-RPC 2.0, one message per line on the socket, or framed after `hello {framed:true}`
  (`hello` returns a `framed` flag plus `channel`-carrying results for streaming methods).
- `hello` returns `generation`, `version`, `core`, `framed`, `methods[]`, `features[]` — i.e. the
  client can discover the live method set and feature flags instead of hardcoding them. This is the
  single most useful fact for #768's "capability negotiation" requirement.
- `hc connect` speaks the same protocol over stdin/stdout and is documented for remote use
  (`ssh box hc connect`) **[help]** — a natural in-process pipe transport for an adapter, without a
  socket path at all.

Session selection **[help / strings]**: `HC_SESSION` (and `hc connect --session`), `HC_SOCKET`,
`XDG_STATE_HOME`, `XDG_CONFIG_HOME` (`config.toml`), and a "use a shorter `HC_SESSION` (or socket in
the config)" diagnostic. The bundled manual states that tests must use their own `HC_SESSION` and
scratch XDG paths. **Not exercised:** I did not start an isolated session, because that would have
started a second core. **[not verified]** the exact socket-name derivation per session.

### 1.2 Windows

**[help / strings]** Windows is a supported, x64-only target:

- The binary embeds `hc-install.ps1`, which installs into `%LOCALAPPDATA%\Programs\herdr-core`
  (`herdr-core.exe` plus **ConPTY** beside it, added to `PATH`, `hc.exe` linked only when no `hc`
  exists) and refuses non-`AMD64` (`windows-x86_64` is the only target). No admin required.
- `hc server status` on Windows never starts a core; a headless Windows machine can still start one
  over SSH; new Windows terminals refresh user/system environment settings including `PATH`.
- Windows-specific semantics appear in the protocol itself: `line` runs through `%COMSPEC% /d /s /c`
  **[schema]**, and terminal `reason` includes `host_shutdown` ("Windows is ending the session,
  including logout") **[schema]**. `keep`/recovery is suspended while "Windows reports session
  shutdown" **[schema]**.
- `machine login` on Windows is "reconnect once its SSH keys work" **[help]**.

**[not verified]** the Windows IPC path and its ACL model. The `named_pipe` string in the binary is
a Rust `io::ErrorKind`, not evidence of a pipe transport, and no path/pipe literal for Windows was
found. Treat Windows socket location, ACLs and multi-user separation as unknown until observed on a
Windows host.

---

## 2. Mapping herdr-mcp's 18-tool contract onto `hc` terminals

The contract is the 18 tools in `crates/herdr-mcp/src/runtime_meta.rs` (`MIGRATED_TOOLS`); the Edge
contract's extra `herdr_devices` action is Edge-local and out of scope here. `hc`'s model is
**terminals** (`t7`, a name, or `machine/t7`) with labels, an optional parent terminal, and a
process-group `foreground`. It has **no workspace, tab, pane or agent object** **[schema]**.

Verdict legend: **equivalent** = same user-visible outcome reachable through a stable hc method;
**partial** = the outcome is reachable but a documented contract field/semantic is missing;
**none** = no hc surface, the tool would have to fail closed.

| # | herdr-mcp tool | what the TUI backend needs | hc surface | verdict | gap |
| --- | --- | --- | --- | --- | --- |
| 1 | `herdr_methods` | live Herdr RPC method inventory | `hc schema` document, `rpc.discover`, `hello.methods[]`+`features[]` | partial | static document / connect-time list, not herdr-mcp's tool contract or epochs |
| 2 | `herdr_inspect` | runtime health, version, workspaces/tabs/panes, live agents, sessions | `server.status` (core id/name, version, pid, uptime, holders, `live`/`finished` counts) + `terminal.list`/`terminal.get` | partial | no workspace/tab/pane graph, no agent registry, no contract epoch, no device/Link state |
| 3 | `herdr_call` | generic, allowlisted Herdr method router with effect classification | raw JSON-RPC line protocol on the socket, or `hc connect` | partial | transport exists; the router, allowlist, preview/fail-closed classification and method normalisation would all be new adapter code |
| 4 | `herdr_since` | resumable event cursor over workspace/pane/agent changes | `events.subscribe`: snapshot + `seq` + typed `event` notifications + 15 s `heartbeat` + `missed`→fresh snapshot | partial | `seq` is a per-core event counter, not a time/state cursor; no workspace/pane/agent event families; terminal `status` is nulled on exit, so post-mortem classification needs prior event capture |
| 5 | `herdr_fs_read` | text file read with bounds | `file.read` (`offset`, `limit`, ≤8 MB, `first`/`lines`) | equivalent **[schema]** | not executed by this spike |
| 6 | `herdr_fs_list` | directory listing with bounded entries | — (only via `machine.exec`/`terminal.run`) | none | no native listing method |
| 7 | `herdr_fs_grep` | bounded content search | — (only via `machine.exec`) | none | no native search method |
| 8 | `herdr_fs_image` | image/binary read for the model | — | none | `file.read` is text-only |
| 9 | `herdr_fs_edit` | exact-once text replacement, all-or-none | `file.edit` (`edits[]`, each `old` found exactly once, non-overlapping, all-or-none, `edit_failed`) | equivalent **[schema]** | not executed by this spike |
| 10 | `herdr_fs_write` | whole-file write | `file.write` (atomic swap, keeps mode, follows symlink) | equivalent **[schema]** | not executed by this spike |
| 11 | `herdr_fs_patch` | unified-diff patch application | — | none | `file.edit` is the only edit primitive; no diff/patch semantics |
| 12 | `herdr_git` | structured, safety-checked git operations | — (only via `machine.exec`/`terminal.run`) | none | no git surface at all |
| 13 | `herdr_exec` | one-shot command, exact stdout/stderr, exit code | `machine.exec` (`argv` or shell `line`, `input` base64, `timeout` default 60 s / max 600 s) | partial (strong) | observed: exit code 3 and stdout/stderr pass through exactly; JSON reports `code`/`stdout`(base64)/`stderr`/`timed_out`/`truncated`/`ms`/`signal`. No incremental output cursor — output is returned once at completion |
| 14 | `herdr_exec_start` | long-running exec session with later reads | `terminal.run` (`--bg`), returns `{address, agent, cwd, name, state, turn}` | partial | a *terminal* (pty + screen) rather than an exec session with a byte cursor |
| 15 | `herdr_exec_read` | incremental output since a cursor | `terminal.read` → `{text, complete, how: screen\|history\|stitched, why}` | partial | screen-shaped text, not a raw byte stream; `--output` is a substring wait, not a cursor |
| 16 | `herdr_exec_kill` | terminate a running exec | `terminal.kill` (hangup → terminate → kill) | partial | observed end state: `reason: killed`, `signal: 1`, `exit_code: null`, `state: finished` |
| 17 | `herdr_prompt` | agent turn submission with target fencing, idempotency, delivery evidence, stop/steer | `terminal.send` (`text`+Enter, `--if idle\|working`, `--wait`, `--force`), `terminal.keys`, `terminal.interrupt` | partial (weak) | observed: `send` **refuses** a `blocked` terminal with `{"error":{"kind":"blocked",…}}` exit 9 unless `--force`; `--if` accepts only `idle`/`working`; `--wait` waits for the turn that send started. No idempotency key, no `target_revision` fence, no delivered/uncertain evidence, no provider-level steer semantics |
| 18 | `herdr_skill` | herdr-mcp's own progressive bootstrap | `templates.list` + run-by-template-name, plus `hc --skill` (its own manual) | none | herdr-mcp's skill bootstrap is runtime-independent; hc's templates are a different concept (local Markdown roles, **empty** on this machine) |

Concepts with **no** hc equivalent at all: workspace, tab, pane, agent instance, agent kind registry,
agent manifests, session/worktree binding, provider/session identity, `herdr_ref_*`. hc has
`labels` (free-form, never interpreted) and `parent` (the terminal that started it) instead —
neither carries identity that herdr-mcp's WebChat binding or continuity could anchor to.

Score across the 18 rows above: **3 equivalent** (`herdr_fs_read`, `herdr_fs_edit`, `herdr_fs_write`
— all `file.*`, schema/help only, none executed by this spike), **9 partial** (`herdr_methods`,
`herdr_inspect`, `herdr_call`, `herdr_since`, `herdr_exec`, `herdr_exec_start`, `herdr_exec_read`,
`herdr_exec_kill`, `herdr_prompt`), **6 none** (`herdr_fs_list`, `herdr_fs_grep`, `herdr_fs_image`,
`herdr_fs_patch`, `herdr_git`, `herdr_skill`).

Of the six `none` rows, five are lost capability and one is not: `herdr_skill` has no hc mapping
because it is runtime-independent (it bootstraps herdr-mcp's own documentation), so it works on any
backend and its `none` verdict costs nothing. That leaves **5 of 18 tools genuinely unservable by
hc**. Either way the score is before counting the missing workspace/tab/pane/agent object graph that
most of the partial rows depend on.

---

## 3. OSC 7501 status semantics vs Herdr TUI agent detection

### 3.1 What each side actually has

**Herdr TUI** (as herdr-mcp consumes it): live agent identity and status are owned by the installed
Herdr stack — `agent.list` is authoritative for live agent instances, status, pane/workspace and cwd;
`server.agent_manifests` is authoritative for the detection manifests; `herdr agent start --help`
declares the startable kinds (`docs/i18n/en/cli-reference.md`, "The scan intentionally does **not**
reimplement Herdr's live-agent detection"). herdr-mcp deliberately never infers agent state itself.

**hc**: status comes *only* from the program's own OSC 7501 report. `Terminal` carries
`status` (`Status` or null), `status_ms` ("when that word last changed"), `reported` ("whether the
program ever sent a 7501 report"), `records` ("other 7501 records by id"), `turn` ("goes up each time
the status leaves `working`"), `title`, and `state` (`live`/`finished`) **[schema]**. `Status` is
`{state: idle|working|done|blocked|error, kind?: permission|question|auth, msg?, title?, progress?
0..100, app?}` **[schema]**, and the bundled manual says plainly: "plain programs report none. Do not
guess status from screen text" **[help]**.

hc has no agent registry, no manifests and no detection heuristics. `hc run`'s `agent` field is just
the argv[0] it launched (**observed**: `{"agent":"sh"}` for `hc run -- sh`), not a detected agent
kind.

### 3.2 What was observed on the wire

**[observed]** Driven through a single disposable interactive `sh` terminal, emitting
`ESC ] 7501 ; state=<word> BEL` on stdout:

| probe | `reported` | `status` | `turn` |
| --- | --- | --- | --- |
| `state=working` | true | `{state: working}` | 0 |
| `state=done` | true | `{state: done}` | 1 |
| `state=working` | true | `{state: working}` | 1 |
| `state=idle` | true | `{state: idle}` | 2 |
| `state=blocked` | true | `{state: blocked}` | 2 |
| `state=error` | true | `{state: error}` | 3 |

All five words are accepted individually. `turn` increments only when the status *leaves*
`working` — matching the schema description.

Multi-field spellings were **not** accepted by this 0.1.14 core. Each of the following left both
`status` and `status_ms` unchanged, i.e. the report was dropped and the previous record retained
(fail-closed at the record level):

`state=blocked;msg=hello` · `state=blocked:msg=hello` · `state=blocked msg=hello` ·
`state=working;progress=50` · `state=blocked;kind=permission` ·
`state=blocked;kind=question;msg=which` · `state=blocked;app=claude` · `state=blocked;` (trailing
`;`) · `msg=hello;state=blocked` · `s1;state=blocked;msg=hello` · `state=blocked;s1;msg=hello`.

This is evidence that *my* spelling of the additional fields was wrong, **not** that `kind`, `msg`,
`progress`, `app` and `records` are unusable — they are in the schema. The authoritative grammar is
not shipped in the binary beyond a hint string (`]7501;?:states=idle,working,done,blocked,error:kinds=permission,question,auth`,
which looks like a capability *query* hc sends to programs, not the report grammar). **Getting the
real 7501 grammar is a prerequisite for any hc backend** (see §6).

One caveat on first observation: a non-interactive `hc run --bg -- sh -c 'printf …OSC…; sleep 5'`
had `reported:false` on two reads ~1 s apart even though the sequence was written, while the same
sequence from an interactive shell after the terminal had settled was accepted. **[not verified]**
whether that is a start-up race, a query/answer ordering requirement, or something else.

### 3.3 Status is a human boundary, and wait/event semantics follow it

**[observed]**

- `hc send <t> …` against a `blocked` terminal → `{"error":{"kind":"blocked","hint":"typing would
  answer its dialog; read its screen first …","message":"<t> is blocked (waiting)"}}`, exit **9**.
  `--force` types anyway ("it may answer its dialog"). herdr-mcp's own policy forbids bypassing a
  human boundary, so an hc backend must never use `--force`.
- `hc wait <t>` when `blocked` → `{"ended":true,"reason":"status","status":"blocked","exit_code":null,"turn":4}`.
- `hc wait <t> --timeout 3` when `working` → `{"ended":false,"reason":"timeout","status":"working"}` —
  a timeout is a check-in, not a failure.
- `hc wait <t> --output READY` → `{"ended":true,"reason":"output","status":"working"}` once the text
  printed after the wait started matched.
- `hc events --json` produced a snapshot line (`{seq, terminals[]}`) then `event` notifications:
  `{"method":"event","params":{"seq":28,"terminal":"t2","type":"status","data":{…reported,records,status}}}`,
  `{"…","type":"turn","data":{"state","turn"}}`.
- After exit, `status` becomes `null` again while `reported` stays `true` and `turn` keeps its value
  (**observed** on the killed terminal: `state: finished`, `reported: true`, `status: null`,
  `turn: 6`).

Consequence for herdr-mcp: a terminal that never reports is treated as `working` until it exits
("A program that never reported status is working until it exits" **[schema]**), and status
disappears at exit. Agent status therefore has to be captured live from `events`, not reconstructed
after the fact.

### 3.4 Which agents emit OSC 7501 today

**[not verified].** This spike launched no real coding agent and `hc templates` was empty
(`{"templates":[]}`), so nothing local shows which agents report. The public claim (issue #767 and
`docs/i18n/en/platform-support-matrix.md`: "reports agent status only through OSC 7501") is a
documentation claim, not local evidence. Listing actual emitters needs a deliberate follow-up probe
on a machine where claude/codex/pi run under `hc`, and it must be re-checked per hc release.

---

## 4. `hc machine add` vs herdr-mcp's device / Link model

**[help / schema / strings]** plus **[observed]** `hc machine ls` → `{"machines":[]}`.

| | `hc machine add` | herdr-mcp Edge device + Link |
| --- | --- | --- |
| transport | **SSH** to another core (`vps`, `user@host`, a `~/.ssh/config` Host), or `--via '<cmd>'` (e.g. `fly ssh console -a x --pty=false -C`) that runs a shell line as its last argument | authenticated **outbound Link** from the device to the Worker; SSH only as explicit maintenance/bootstrap |
| identity | this core's name for the other core (`machine/t7`) + the other core's permanent random `id`, its self-reported `host` and `version` | immutable `dev_*` `device_id`, device credential, generation fencing, device-bound `herdr_ref_*` |
| credential ownership | OpenSSH owns auth (`ControlMaster`/`ControlPath` strings in the binary); a remote that needs a password/code uses `hc machine login` | owned credential store (macOS Keychain / Windows Credential Manager / Linux secret store) |
| pairing | `machine.add` dials, verifies that hc answers, stores the profile; `--install` copies *this* hc to the remote when it has none or one too old | enrollment → device credential → Link reconciliation |
| failure surface | `unreachable`, `hc_missing`, `incompatible` (`name_taken`, `already_added`); `machine ls` shows `connecting`/`connected`/`needs_login`/`unreachable` + `since_ms` + `why` | `delivery_state`, Link readiness, generation fencing, per-mutation evidence |
| ownership of remote work | `machine rm` "forgets" it locally — its terminals keep running there; `machine update` copies hc over and restarts it remotely (terminals kept) | managed runtime generations, activation/rollback, cleanup ownership |
| short-lived hosts | `--temp` marks a sandbox so clients can display it apart; "hc treats it like any other" | reclaim/cleanup policy with deterministic `safe_to_delete` |

Reading: `hc machine add` is structurally the same *namespace* as a saved Herdr SSH machine profile
(label + session + transport + version, no copied credentials), **not** the Edge device/Link identity
model. Three consequences for #768:

1. Enrolling an "hc machine" would create a fourth identity namespace (Herdr TUI machine profile,
   Edge `dev_*`, hc `machine/<name>` + per-core random id); no hc value can be inferred from or
   translated into `dev_*`/`herdr_ref_*`.
2. `--install` (copy this hc to the remote and restart it) and `machine update` are **remote binary
   mutations**, exactly the class of operation herdr-mcp keeps behind explicit operator paths. An
   adapter must not inherit them.
3. hc's reachability states are transport facts (`unreachable`/`needs_login`), not delivery evidence;
   herdr-mcp's uncertain-delivery rules cannot be derived from them.

---

## 5. Schema versioning and stability

**[observed]** from `hc schema`:

- OpenRPC **1.3.2** document; `info.version` = `0.1.14` = the **product release version** (`hc
  --version` prints the same string). There is no separate contract epoch/version field.
- `info.description` states: "JSON-RPC 2.0, one message per line on the hc socket (or framed after
  `hello`). **Protocol generation 1.** Errors carry a stable `data.kind`."
- 32 methods / 34 component schemas, each method documented with `description`, typed `params`
  (`paramStructure: by-name`, defaults, bounds) and its own `errors[]` with JSON-Schema `data.kind`.
- Compatibility is negotiated, not versioned per method:
  - `hello {generation}` → `{generation, version, core, framed, methods[], features[]}`, or error
    `-32000 unsupported_generation`.
  - `machine.add` returns `incompatible` when the remote "speaks another protocol generation".
  - `hc update --force` is required to move to "a release of another protocol generation", and
    "saved state only moves forward".
- Errors are stable in shape: `{error:{kind, message, hint}}` on stderr/JSON; observed kinds
  include `blocked` (exit 9) and `usage` (exit 2); documented exit codes 3 `not_found`,
  7 `needs_target`, 9 `blocked`, 10 `status_mismatch`, 11 `finished` **[help]**.
- `doc.set` uses an explicit version compare-and-swap for its opaque JSON documents **[schema]** —
  the only per-object optimistic-concurrency primitive in the API.

Assessment: the **generation handshake plus connect-time `methods[]`/`features[]` is a real
capability-negotiation mechanism** and is the right thing for #768 to consume — it is much better
than sniffing the version string. But there is **no documented N/N-1 promise, no deprecation policy
and no independent schema version** in the 0.1.14 material I could read; method sets can change
between releases and the only guard is generation bump + `incompatible`/`unsupported_generation`.
**[not verified]** whether herdr.dev/core publishes a stability policy beyond this.

Practical implication: pinning an hc contract means pinning an hc *release* (0.1.14 today), the same
way the TUI lane pins Herdr 0.9.5 — and the adapter must re-read `hello.methods[]` on every connect
rather than trusting a compiled table.

---

## 6. Go/no-go input for #768

**No-go for "hc as a drop-in backend behind the unchanged 18-tool contract."** Five of eighteen tools
have no hc surface at all (`fs_list`, `fs_grep`, `fs_image`, `fs_patch`, `herdr_git`), the
workspace/tab/pane/agent object graph that most other tools depend on does not exist in hc, agent
identity/status cannot be obtained (no `agent.list` equivalent, no manifests), and `herdr_prompt`'s
reliability kernel (idempotency key, target fence, delivery evidence, steer/stop semantics) has no
counterpart — `terminal.send` is a paste-and-Enter with a human-boundary refusal.

**Conditional go for a deliberately narrow, fail-closed adapter**, and only if #768 explicitly
accepts a reduced capability set:

- Implement only what is genuinely there: terminal lifecycle (`run`/`list`/`get`/`read`/`kill`/`rm`/
  `label`/`keep`), `machine.exec` for one-shot exec, `file.read|write|edit`, `events.subscribe`.
- Everything else returns a stable `unsupported` error (not a guess, not a screen scrape).
- Runtime selection stays explicit per device with **no fallback** between runtimes — they do not
  share agents or terminals, so a fallback would silently change the answer.
- Drive capability negotiation from `hello.methods[]`/`features[]`/`generation`, never from a
  hardcoded method table; re-negotiate on every connect.
- Keep `--force` unreachable from the adapter (human boundary), and keep `machine add --install` /
  `machine update` out of the product surface.

### Prerequisites before any 1.2 implementation commitment

1. Obtain the authoritative **OSC 7501 report grammar** (multi-field spelling, record ids,
   query/answer handshake) from herdr.dev/core. Without it, `kind`/`msg`/`progress`/`records` — the
   fields that carry a blocked reason — cannot be relied on. My probe could only prove
   `state=<word>`.
2. Verify **which real agents emit 7501 today**, per agent and per version. If the common agents do
   not emit it, hc has no agent status at all and the value of an hc backend collapses to
   "terminals + exec".
3. Observe **Windows** IPC path/ACL and confirm whether the filesystem-only auth model holds there.
4. Confirm hc has no agent-kind registry/manifest surface in later releases (0.1.14 has none).
5. Decide the contract question in #768: if the 18-tool contract stays fixed, the hc backend can only
   ever serve a small subset and the "fail closed" behaviour becomes the dominant user experience;
   if it needs an epoch change, that is a much larger decision than an adapter refactor.

### Risks (ranked)

1. **Agent status cannot be reconstructed.** Status is program-reported and optional; unreported means
   `working` until exit, and status is nulled at exit. Any herdr-mcp view of "what is the agent doing"
   would be strictly weaker than the TUI lane, with no detection fallback (hc forbids guessing from
   screen text).
2. **Human-boundary collision.** `blocked` means "a person is needed"; `send --force` can answer the
   dialog. The adapter must hard-refuse, matching herdr-mcp's existing human-boundary rule.
3. **Weaker auth boundary.** Filesystem ownership only, no token, no per-client identity; unlike the
   Edge/Link model it gives no revocation, no delivery evidence and no device binding. Multi-user and
   Windows behaviour unknown.
4. **New remote-mutation surfaces.** `machine add --install` and `machine update` copy and restart
   binaries on other hosts over SSH — outside herdr-mcp's managed runtime ownership rules.
5. **Contract-pinning cost.** Schema version == product version; no N/N-1 promise found. Supporting
   hc means tracking hc releases and generation bumps.
6. **No workspace/pane anchor.** WebChat binding, continuity and `herdr_ref_*` identity have nothing
   to attach to in a terminal-only model; labels are free-form and uninterpreted.
7. **Event resynchronisation.** `seq` is per-core with a 15 s heartbeat and `missed` → full snapshot;
   an adapter must handle snapshot resets rather than assuming a durable stream.
8. **No idempotency primitive for sends.** `terminal.send` has `--if idle|working` and `--wait`, but
   no request id and no dedupe record, so uncertain delivery cannot be resolved the way
   `agent.prompt` operations are.

Relationship to 1.1: none of this changes shipped behaviour. The #765 presence warning is already
implemented (`crates/herdr-mcp/src/herdr_core.rs`, warning code `herdr_core_unsupported`) and stays
correct — this spike confirms that "unsupported" is the right 1.1 position.

---

## 7. Captured schema (redacted)

`hc schema` prints one JSON document, 63,134 bytes, sha256
`7f830922f96da848ec151de10349abddf3233c74a06c00c25b2bd58f448c6b11`.

Redaction check run against the raw capture before quoting it: zero occurrences of the account name,
the macOS home prefix, the host name, the core `id`, the platform `machine_id`, the peer id, the
temporary directory prefix, or the primary group. The document contains no absolute paths and no per-machine values at all — it is a
pure API description, so the excerpts below are quoted verbatim.

Top level: `{components, info, methods, openrpc}`; `openrpc: "1.3.2"`.

`info`:

```json
{
  "description": "Terminal runtime for coding agents. JSON-RPC 2.0, one message per line on the hc socket (or framed after `hello`). Protocol generation 1. Errors carry a stable `data.kind`.",
  "license": {"name": "Apache-2.0"},
  "title": "herdr-core",
  "version": "0.1.14"
}
```

`components.schemas` (34): `AttachResult, Children, CoreInfo, Display, DocGetResult, DocSetResult,
Edit, ExecResult, FileReadResult, FileResult, FirstMessage, Foreground, HelloResult, HolderInfo,
IdleOrWorking, InterruptResult, KeysResult, ListResult, Machine, MachineListResult, MachineView,
PutResult, ReadResult, RemoveResult, RunResult, SendResult, Status, StatusResult, StopResult,
SubscribeResult, TemplateInfo, TemplatesResult, Terminal, WaitResult`.

`methods` (32, verbatim names and parameter names):

| method | params |
| --- | --- |
| `hello` | `calls_you, core, framed, generation` |
| `server.status` | — |
| `server.stop` | — |
| `server.restart` | — |
| `terminal.run` | `argv, caller, cwd, die_with_parent, env, keep, labels, line, name, no_notify, no_parent, on, outlive_parent, parent, rm, timeout, wait` |
| `templates.list` | — |
| `terminal.list` | `all, labels` |
| `terminal.get` | `caller, terminal` |
| `terminal.kill` | `caller, terminal` |
| `terminal.rm` | `caller, terminal` |
| `terminal.keep` | `keep, terminal` |
| `terminal.label` | `labels, terminal` |
| `terminal.read` | `caller, lines, terminal` |
| `terminal.attach` | `caller, cols, display, history, observe, resumed, rows, terminal` |
| `terminal.send` | `caller, force, if, terminal, text, timeout, wait` |
| `terminal.wait` | `caller, children, output, terminal, timeout` |
| `terminal.keys` | `caller, keys, terminal` |
| `terminal.interrupt` | `terminal, timeout` |
| `terminal.put` | `data, name, terminal` |
| `machine.exec` | `argv, cwd, env, input, line, on, timeout` |
| `file.read` | `cwd, limit, offset, on, path` |
| `file.write` | `cwd, data, on, path` |
| `file.edit` | `cwd, edits, on, path` |
| `doc.get` | `name, on` |
| `doc.set` | `name, on, value, version` |
| `events.subscribe` | `children_of, labels, terminal` |
| `machine.add` | `name, session, target, temporary, via` |
| `machine.list` | — |
| `machine.connect` | `machine` |
| `machine.rename` | `machine, new_name` |
| `machine.rm` | `machine` |
| `rpc.discover` | — |

The five schemas that carry this document's semantics, quoted in full:

```json
{"Status": {
  "description": "One 7501 record. Words are the program's own; nothing is translated.",
  "properties": {
    "app": {"type": "string"},
    "kind": {"description": "permission, question or auth (blocked only, when the program said).", "type": ["string", "null"]},
    "msg": {"type": "string"},
    "progress": {"description": "0..100 (working or blocked, when the program said).", "format": "uint8", "maximum": 255, "minimum": 0, "type": ["integer", "null"]},
    "state": {"description": "idle, working, done, blocked or error.", "type": "string"},
    "title": {"type": "string"}
  },
  "required": ["state"], "type": "object"}}
```

```json
{"HelloResult": {
  "properties": {
    "core": {"$ref": "#/components/schemas/CoreInfo"},
    "features": {"items": {"type": "string"}, "type": "array"},
    "framed": {"type": "boolean"},
    "generation": {"format": "uint32", "minimum": 0, "type": "integer"},
    "methods": {"items": {"type": "string"}, "type": "array"},
    "version": {"type": "string"}
  },
  "required": ["generation", "version", "core", "framed", "methods", "features"], "type": "object"}}
```

```json
{"WaitResult": {
  "properties": {
    "children": {"description": "With `children`: one result per child it reports on.", "items": {"$ref": "#/components/schemas/WaitResult"}, "type": "array"},
    "ended": {"description": "False on timeout: a check-in, the terminal is still running.", "type": "boolean"},
    "exit_code": {"format": "int32", "type": ["integer", "null"]},
    "from": {"description": "With a message: the sender's address (the text is already in your input); null when a person sent it.", "type": ["string", "null"]},
    "reason": {"description": "status, exit (including program loss in a kept terminal), output, timeout, or not_started …", "type": "string"},
    "signal": {"format": "int32", "type": ["integer", "null"]},
    "status": {"description": "The exact word the program reported (current word on timeout or output).", "type": ["string", "null"]},
    "terminal": {"type": "string"},
    "turn": {"format": "uint64", "minimum": 0, "type": "integer"}
  },
  "required": ["terminal", "ended", "reason", "turn", "children"], "type": "object"}}
```

```json
{"ReadResult": {
  "properties": {
    "complete": {"description": "False when part of what was asked for may be missing.", "type": "boolean"},
    "how": {"description": "\"screen\", \"history\", or \"stitched\" (a full-screen app, read by scrolling it).", "type": "string"},
    "text": {"type": "string"},
    "why": {"description": "Why only the screen: \"working\" (a full-screen app isn't scrolled while it works; read again once it stops).", "type": ["string", "null"]}
  },
  "required": ["text", "complete", "how"], "type": "object"}}
```

```json
{"Terminal": {
  "required": ["address", "labels", "argv", "cwd", "state", "created_ms", "screen", "cols", "rows"],
  "properties": {
    "address": {"description": "`t7`: this terminal forever on this core.", "type": "string"},
    "state": {"description": "\"live\" or \"finished\".", "type": "string"},
    "reported": {"description": "Whether the program ever sent a 7501 report.", "default": false, "type": "boolean"},
    "status": {"description": "The program's own word for what it's doing (OSC 7501 root record); null if it never said, or after it exited.", "anyOf": [{"$ref": "#/components/schemas/Status"}, {"type": "null"}], "default": null},
    "status_ms": {"description": "When that word last changed, ms since the epoch; null if it never said.", "...": "uint64|null"},
    "records": {"description": "Other 7501 records by id (programs reporting on several things).", "additionalProperties": {"$ref": "#/components/schemas/Status"}, "type": "object"},
    "turn": {"description": "Goes up each time the status leaves `working`.", "...": "uint64"},
    "foreground": {"description": "What runs in it right now and where (null once it has finished).", "anyOf": [{"$ref": "#/components/schemas/Foreground"}, {"type": "null"}]},
    "labels": {"additionalProperties": {"type": "string"}, "type": "object"},
    "parent": {"description": "The terminal that started it (`t3`), set once at start.", "type": ["string", "null"]},
    "keep": {"description": "Replace a lost program with this machine's shell, in place. Exit with a code and kill remain final.", "default": false, "type": "boolean"},
    "reason": {"description": "Why its program ended or is pending …: exited, killed, server_stop, host_shutdown (Windows is ending the session, including logout), signaled, holder_lost, stopped_by_restart.", "type": ["string", "null"]}
  },
  "note": "properties abbreviated with \"...\" where the omitted part is only a format string"}}
```

(The full 63 KB document was reviewed locally and is intentionally not committed: `.gitignore`
treats `docs/_wip/*` as local evidence dumps, and the API surface above is the part #768 needs.)

---

## 8. Commands run

All commands ran with `http_proxy`/`https_proxy`/`ALL_PROXY` unset and from the spike worktree
(`git rev-parse --show-toplevel` → this worktree; base `origin/main` = `794f31c7`). Read-only unless
marked *(mutating: own disposable terminal only)*.

Read-only discovery:

```
hc --version
hc --help
hc --skill                      # bundled manual, 168 lines
hc server --help ; hc schema --help ; hc connect --help
hc machine --help ; hc machine add --help ; hc machine ls --help
hc update --help ; hc send --help ; hc wait --help
hc server status                # {"core":{…},"holders":[],"live":0,"finished":0,"version":"0.1.14",…}
hc machine ls --json            # {"machines":[]}
hc schema > schema-raw.json     # 63,134 bytes, sha256 7f830922…
shasum -a 256 schema-raw.json hc-skill.txt
lsof -a -U -p <core pid>        # shows the Unix socket path
stat / ls -la $HOME/.local/state/herdr-core{,/default}
ps -o pid,user,etime,command -p <core pid>
strings -a ~/.local/bin/herdr-core | grep …   # 7501, windows, sock, HC_SESSION, install.ps1
hc exec -- sh -c 'printf "out\n"; printf "err\n" >&2; exit 3'   # exit 3 passed through
hc exec --json -- sh -c 'printf "hello\n"'                     # base64 stdout/stderr + code/ms
```

*(mutating: own disposable terminal only)* terminal lifecycle, named `probe767`, removed at the end:

```
hc run --bg --json --name probe767 -- sh
hc get probe767 --json ; hc read probe767
hc send [--force] probe767 "sh <emit.sh> 'state=…'"     # emit.sh prints ESC ] 7501;PAYLOAD BEL
hc wait probe767 --json [--timeout N] [--output READY]
hc events --json > events.jsonl &                       # 4 lines captured, then killed
hc kill probe767 ; hc rm probe767
hc ls --all                                             # {"hidden_finished":0,"terminals":[]}
```

Repository checks (node_modules absent in this worktree, so no `npm ci`, no `build:site`, no docs
`node --test`):

```
node scripts/lint-findings.mjs        # PASS (14 findings, 1 rule)
node scripts/i18n-doc-parity.mjs      # ja: parity checks pass
git check-ignore -v docs/_wip/herdr-core-probe.md
```

Confirmed that the docs lane cannot see this file: `scripts/i18n-doc-parity.mjs` reads only
`docs/i18n/<locale>`, `scripts/build-site.mjs` only `docs/i18n` (plus `site/`, `assets/`), and
`tests/site-build.test.mjs` explicitly asserts that generated docs contain no `_wip` entry
(`assert.equal(generatedDocs.some((name) => name.includes("_wip")), false)`). `.gitignore` un-ignores
`docs/_wip/*.md` (`!docs/_wip/*.md`), so this Markdown file is tracked while a `.json` dump beside it
would not be.

---

## 9. Not verified

Recorded so #768 does not inherit an assumption as a fact:

1. OSC 7501 multi-field grammar (`kind`, `msg`, `title`, `progress`, `app`, record ids, the
   `]7501;?` query/answer handshake). Only `state=<word>` was proven to work.
2. Why a non-interactive `sh -c 'printf …OSC…; sleep'` did not register `reported` while an
   interactive shell did (race vs. ordering).
3. Which real agents emit 7501 today, per agent and version.
4. Windows IPC path, ACL model and any multi-user separation; also whether ConPTY changes `read`
   semantics.
5. Per-session socket naming (`HC_SESSION`/`HC_SOCKET`/`XDG_STATE_HOME`) — not exercised, because it
   would have started a second core.
6. `file.read|write|edit` behaviour (schema/help only — executed nothing remote or local-destructive).
7. `terminal.attach` / framed-mode streams / `terminal.put` / `terminal.keys` / `terminal.interrupt`.
8. Any `hc machine add` behaviour: help/schema only, deliberately not executed (`{"machines":[]}`).
9. `hc update`, `server stop`, `server restart` — never run; the pre-existing core was left untouched.
10. Whether herdr.dev/core publishes a stability/N-1 policy beyond the generation handshake.
11. hc behaviour under the Herdr app (Ghostty front-end) versus headless `hc run`; this spike only
    saw headless terminals (`identity: "ghostty"` was the inherited `TERM_PROGRAM`, not a front-end).
