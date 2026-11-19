# Design: tcc_broker exec mediation (issue #1008)

Status: **design proposal — documentation only**. This document proposes a bounded
`exec` mediation operation for `tcc_broker` on macOS. It does not authorize any code
change. Per issue #1008, `crates/herdr-mcp/src/tcc_broker.rs`,
`crates/herdr-mcp/src/utility_exec.rs`, and the signing scripts MUST NOT be modified
by this issue; only this design document is committed.

## 1. Background

`utility_exec` (`herdr_exec` / `herdr_exec_start` / `herdr_exec_read`) currently
chooses among three backends per call (`crates/herdr-mcp/src/utility_exec.rs`):

- `native` — `run_native_durable` / `run_oneshot_readonly`: in-process
  `ExecRegistry` spawn with an OS pipe. Used when
  `project_path_needs_protected_transport(&effective_root)` is false.
- `utility_pane` — `run_unix_durable`: creates a durable utility pane inside the
  user's Herdr workspace and drives it with prompt/read cycles. Used when the
  effective root is protected (`crates/herdr-mcp/src/macos_permissions.rs:429`
  `project_path_needs_protected_transport`), because the runtime's own process
  cannot `cd` into or read a TCC-protected directory.

`project_path_needs_protected_transport` returns true when the path itself is
TCC-protected (`is_protected_user_path`: `~/Documents`, `~/Desktop`,
`~/Downloads`, etc.) **or** when the path is a linked git worktree whose `.git`
file resolves its `gitdir:` under a protected directory. On macOS, virtually
every managed project root falls into one of these two cases:

| Managed root (macOS device A, 2026-11-18) | Why protected |
| --- | --- |
| `~/Documents/herdr-mcp` | under `~/Documents` |
| `~/.herdr/worktrees/herdr-mcp/*` | `.git` file → `gitdir:` under `~/Documents/herdr-mcp/.git/worktrees/...` |
| `~/Documents` (unmanaged project) | under `~/Documents` |

Consequently `herdr_exec` on a real macOS workstation is `utility_pane` for
effectively 100% of calls today — the `native` backend is unreachable through
the managed exec surface (a `herdr_exec_start` with `root=/tmp` is refused with
`outside_managed_roots`).

### Why a broker exec is proposed

The utility_pane fallback is correct for TCC: only a process that runs inside a
pane already granted Full Disk Access (or per-directory TCC grants) may touch
protected paths. But the durable pane path has costs documented below
(§5 measured baseline): pane creation, shell prompt synchronization, screen
scraping for output, and cleanup. A TCC-holding `tcc_broker` child process —
the same trust mechanism already used for `fs_*` / `git_status` operations —
could execute a bounded exec request over a pipe instead, avoiding the pane
entirely for read-only / well-formed commands.

`tcc_broker` today is deliberately **not** a generic shell: it dispatches only a
strict allowlist of bounded fs/git operations (`tcc_broker.rs:16-20`, enforced
in `parse_request` at `tcc_broker.rs:833+` — `unknown operation '{op}'` for
anything outside the allowlist). Any `exec` addition must preserve that
invariant: the broker must never gain arbitrary `sh -c <attacker string>`.

## 2. Threat model and privilege amplification surface

The broker runs with the user's TCC grants (FDA or scoped folder grants).
Adding `exec` creates the following amplification surfaces:

| Threat | Current state | If `exec` is added |
| --- | --- | --- |
| Remote/Edge caller sends arbitrary command to a TCC-holding process | Impossible — allowlist rejects unknown ops; no op accepts a shell string | **New surface**: the broker's child inherits the broker's TCC grants. A command string reaching the broker would run with FDA-level rights. This is the single largest amplification and the reason the op must be constrained to a command *template allowlist* or a flag-restricted program, not raw argv. |
| Compromised broker binary | Signed binary + `inspect_code_identity` / `installed_compat_state` checks | Unchanged, but impact grows: a tampered broker that can exec is a full FDA backdoor. Signing/identity verification becomes mandatory, not advisory. |
| Local same-user process tries to borrow the runtime's authority | **No broker socket exists today.** The runtime spawns the installed broker as a one-shot child and hands it the request over stdin; a same-user process that executes the broker binary itself is indistinguishable from that child and gets exactly the *current* fs/git allowlist — no runtime authority, no `exec`, and the `HERDR_TCC_BROKER_EXEC` fence still applies | **New surface**: once `exec` exists and a broker daemon socket is considered, caller identity must be authenticated by the future layers in §3 (0700 socket dir + peer credentials + peer image + `runtime_owner_token` binding). None of that exists today. |
| Request bypasses workspace scoping | fs ops are path-validated under managed roots | An exec op must bind the working directory to the managed roots set and reject `cwd` escapes, `..`, symlinks out of the root. |
| Environment variable injection (`DYLD_INSERT_LIBRARIES`, `PATH`, `SHELL`, `HOME` redirection) | N/A today | Broker child env must be a fixed minimal set (§4). `DYLD_*` on the broker's *own* process is already neutralized by signing + SIP; the child env is a fresh attack vector. |
| Shell metacharacter / argv smuggling | N/A | If templates are allowed, template argv must be literal-token-only; no second-shell expansion. Prefer fixed-program + flag-subset over free argv. |
| Output exfiltration / unbounded output | fs ops return bounded JSON | Exec output must be size-capped (`GREP_MAX_*`-style), spill to the existing spool only, and count skipped bytes. |
| Runaway child after timeout | ExecRegistry reaps native children | Broker must reap the full process group (`setsid` + `killpg`) and audit the kill (§6). |
| Denial-of-service via exec flood | Link applies rate/lease budgets | Broker needs a per-caller concurrency cap (e.g. 4) + queue bound; without it, a busy loop could pin TCC-holding CPU/IO. |

**Privilege amplification summary:** today the broker is not a server. The
runtime runs the installed broker binary as a one-shot child over pipes, so the
only reachable surface is the existing fs/git allowlist applied to managed roots
— for the runtime *and* for any same-user process that runs the binary itself.
With `exec`, whoever can reach the broker could read/write/execute *anything*
the user's TCC grants cover —
an amplification from managed-root FS to all-protected-paths + arbitrary code.
This is only acceptable if the executed surface stays at or below the semantic
power of the current fs/git allowlist (i.e. bounded, non-shell operations), or
if every command is constrained to commands the runtime could already execute
itself unprotected.

## 3. Caller identity verification

### 3.1 What exists today (parent→child pipe boundary)

There is **no broker socket, no listener and no peer-credential check**. The
runtime executes the installed broker as a one-shot child
(`crates/herdr-mcp/src/tcc_broker.rs::run_broker_child`), writes one bounded
request to the child's stdin and reads one JSON response from stdout
(`run_broker_once` → `parse_request`, which accepts exactly
`protocol`/`version`/`op`/`snapshot`/`args`). What is enforced:

1. **Spawn-time binary identity.** The broker path must be a regular file, not a
   symlink; `inspect_code_identity` / `installed_compat_state` /
   `installed_compat_revision` verify signature/TeamID and protocol revision
   *before* the runtime delegates. This answers "did we spawn the binary we
   expect", not "who called".
2. **Recursion fence.** `is_broker_child_process` (`HERDR_TCC_BROKER_EXEC=1`)
   stops a broker child from re-entering protected-path routing.
3. **Request-shape allowlist.** Exact top-level keys, one allowlisted `op`, and
   fs ops still path-validate under managed roots (unchanged behaviour).

Consequence to state plainly: a same-user process that runs the installed broker
binary directly looks exactly like the runtime's child to the broker, and gets
the same allowlist. That is acceptable only while the allowlist stays at the
current fs/git power; it is **not** an identity boundary for a future `exec`.

### 3.2 Required before tier-1 `exec` (or before any broker daemon socket)

None of the following exists; each is a design-and-implementation precondition,
tracked with the token item in §9:

1. **Socket directory + requester peer credentials.** If the broker grows a
   daemon socket, it must live under `~/.config/herdr-mcp/` with 0700 and verify
   `LOCAL_PEERCRED` (pid) plus the peer's executable image path
   (`runtime/current/herdr-mcp` via `csops`/proc_pidinfo), not merely same-uid.
2. **`runtime_owner_token` binding and rotation.** The request would carry the
   runtime's per-instance owner token, the broker would compare it with the
   runtime owner record on every request, a mid-connection rotation would fail
   closed, and a restart would load only the current owner record (§9).
3. **Replay protection.** Request ids monotonic per runtime generation, with a
   broker-side seen-request table (§9 item).
4. **Rate limiting.** Per-caller concurrency cap and a global exec ops/sec
   ceiling; violations audited (§5), not just refused.

Until 1–4 are implemented together, enabling `exec` would put arbitrary
TCC-scoped execution behind a boundary that authenticates nothing beyond "this
is some same-user process running our signed binary".

Non-goals: the broker does not authenticate the *Edge caller* end-to-end; the
runtime remains the accountability boundary. The broker's job is to refuse
anything that did not come from the verified managed runtime.

## 4. Managed-repository allowlist and argument/environment sanitization

### 4.1 Root allowlist

- The request carries `project_root`. Broker resolves it (`realpath`) and
  requires membership in the *runtime-provided* managed-roots set for this
  request (same list `herdr_exec_start` enforces via `outside_managed_roots`),
  and additionally requires `is_protected_user_path(root)` — an unprotected
  root must not reach the broker at all (the runtime should use `native`;
  broker exec for unprotected roots is rejected to keep the TCC surface
  minimal).
- `cwd` must equal `project_root` or resolve inside it; absolute `cwd`
  outside the root, `..` escapes, and symlinks resolving outside are rejected.
- The git-worktree rule applies to `cwd` too: a `.git` *file* (linked
  worktree) whose `gitdir:` resolves under a protected path counts as
  protected — this is already enforced by
  `project_path_needs_protected_transport` on the runtime side; the broker
  must re-implement the check locally and not trust the caller's claim.

### 4.2 Command policy — two explicit tiers

**Tier 1 — fixed-program allowlist (recommended for v1):** a program-name
allowlist (`git`, `rg`, `sed`-free file readers, `ls`, `find` with bounded
depth, `cat` with size cap, `pwd`, `stat`, `xcodebuild`-class tools *not*
included) plus a per-program flag blacklist (`git`: deny `-c`, `--exec-path`,
`--git-dir`, `-C` outside root, `config`, `clean -fdx`, `apply`, `stash`,
`push`, `update-index --cacheinfo`...; `find`: deny `-exec`/`-delete` unless a
future tier). No shell, no `|`, `;`, `&&`, `$()`, backticks, redirects except
into spool-path inside root. This covers ~90% of protected-path exec traffic
(git status/log/diff, rg, file reads) at the same semantic power as today's
fs/git ops.

**Tier 2 — runtime-signed command descriptors (future, opt-in):** the runtime
computes a canonical descriptor (program + argv + cwd + env-digest), signs it
with a per-session key the broker fetched at bind time, and the broker
executes the literal argv directly (no shell). This supports arbitrary
programs but only those the managed runtime itself constructed — preserving
"runtime is the accountability boundary". Deferred; requires key plumbing.

**Never:** `shell:true`, `bash -c`, `zsh -c`, `env`, `eval`, or a `command`
string the broker tokenizes itself.

### 4.3 Environment sanitization

Child env = fixed allowlist only:

```
PATH=<fixed runtime-managed path list>   # never caller-provided
HOME=<requester's $HOME>
LANG / LC_ALL / TZ                       # locale only
TMPDIR=<broker-created 0700 dir>
HERDR_TCC_BROKER_EXEC=1                  # marker for children to detect broker context
```

Explicitly dropped: `DYLD_*`, `PATH`/`SHELL`/`BASH_ENV`/`ENV`/`CDPATH`/`IFS`/
`GLOBIGNORE`/`LD_*`/`RUST_BACKTRACE`(allowed but bounded)/any `*_PROXY`/
credential-bearing vars. The broker builds the env; the request may not add
keys in v1.

### 4.4 I/O and output bounds

- stdout/stderr → spool files under `<root>/.herdr/exec-spool/` (or broker
  tmp), size-capped (`MAX_EXEC_OUTPUT_BYTES`, default 4 MiB); over-cap output
  is truncated with `truncated:true` and `bytes_total` reported — same shape
  as `ExecRegistry` progress today.
- Return shape mirrors `run_oneshot_readonly`/`run_unix_durable` results:
  `{ok, exit_code, output, truncated, backend:"tcc_broker", session_id}`.

## 5. Timeout reaping and audit logging

- **Timeouts:** request carries `timeout_ms` (cap 300s; broker clamps, never
  extends). On expiry broker sends SIGTERM to the process *group* (child
  spawned via `setsid`), waits 2s, SIGKILL, reaps. Orphan sweep: broker scans
  its child table every 30s and reaps groups older than 2× their timeout.
- **Spool cleanup:** spool files older than 24h are deleted on broker start
  and hourly thereafter.
- **Audit log:** append-only JSONL at
  `~/.config/herdr-mcp/broker-audit/<YYYY-MM-DD>.jsonl`, one line per request:
  `{ts, request_id, peer_pid, peer_image, op:"exec", program, argv_len, cwd_root, outcome, exit_code, duration_ms, output_bytes, killed}`.
  `peer_pid` / `peer_image` only become meaningful once §3.2 layer 1 exists; today
  the broker is a pipe child, so the audit line records the parent runtime pid
  (nothing about an independent caller).
  Program name and argv *length* are logged; argv content is logged only at
  `debug` level and never includes env values. Audit entries are what make
  the amplification reviewable.
- **Kill semantics for pane parity:** `herdr_exec_kill` must route to the
  broker for broker-backed sessions (`session_id` prefix distinguishes
  backends).

## 6. Measured baseline: utility_pane vs native backend

Requirement (issue #1008): real measured latency distributions for the
protected-path fallback vs the unprotected direct-pipe path, with commands and
raw samples — no unattributed estimates.

### 6.1 Method

Command measured: `printf ok` (trivial command; isolates transport/backend
overhead). Two timings per call:

- **E2E**: wall time of the full `herdr_exec` MCP call (Edge relay + backend).
- **internal**: `progress.elapsed_ms` reported by the runtime (backend-side
  command execution window).

Invocation (one per sample):

```
mcp herdr_exec {workspace, project_root, command:"printf ok", timeout_ms:30000, confirm_busy:true, device:<dev_id>}
```

### 6.2 Raw samples

**Collection environment and time.** Samples were collected on 2026-11-18 on
two macOS workstations (TCC-protected vs. home root) with the `herdr-mcp`
runtime at `v1.2.0` and the pinned Herdr TUI `0.9.5`. The two utility-pane rows
are 10 consecutive measured runs each after one warmup. The native rows are
**not** the same length: the E2E array holds 9 measured runs plus a separately
reported warmup (~4500 ms), while its `internal ms` array holds 10 values — the
original capture kept one E2E sample out of the table, so compare medians and
outlier counts rather than pairing E2E with `internal` element by element. Re-measure before treating these
numbers as a current baseline — runtime and Herdr versions both affect the
utility-pane overhead, and this table is evidence for the design trade-off, not
a release gate.

**Protected path — `backend:"utility_pane"`**, macOS device A
(`dev_<device-A>`), workspace `<ws-A>`,
root `~/Documents/herdr-mcp`:

```
E2E ms:      [2055, 1526, 2717, 2112, 2294, 1493, 2072, 1439, 1629, 2054]
internal ms: ~35–56 (pane-side exec window; sample field observed 35–56)
```

Same command, linked worktree root
`~/.herdr/worktrees/herdr-mcp/<worktree>`
(workspace `<ws-B>`) — *also* `utility_pane` because its `.git` file resolves
`gitdir:` under `~/Documents`:

```
E2E ms:      [1502, 1568, 1508, 1546, 1429, 1551, 1568, 1447, 1451, 1495]
```

**Unprotected path — `backend:"native"`**, macOS device B
(`dev_<device-B>`), workspace `<ws-C>`, root `~`
(home root, not a TCC-protected folder):

```
E2E ms:      [3500, 3046, 31951, 12166, 15821, 3162, 2922, 2860, 3739] + warmup ~4500
internal ms: [60, 70, 70, 61, 71, 61, 61, 89, 71, 61]
```

### 6.3 Reading the data

- Internal exec cost is comparable across backends (pane ~35–56 ms vs native
  ~60–97 ms on these samples): the per-command backend difference is tens of
  milliseconds, well under Edge relay latency (~1.5–2.7 s E2E median on
  device A, ~3 s median on device B, with multi-second tail outliers on the
  native path).
- The utility_pane cost that matters is **not per-command exec time** but the
  durable-pane lifecycle: pane creation (first call to a workspace can take
  several seconds), prompt-sync waits, and pane occupancy — costs that scale
  with concurrency and long sessions, not with individual `printf` calls.
  These were not exercised by the `printf ok` microbenchmark.
- **Measurement limitation (documented honestly):** on device A
  itself, *no* managed root resolves to an unprotected path (§1 table), so
  `backend:"native"` cannot be exercised on that machine through the managed
  surface — `herdr_exec_start` with `root=/tmp` returns
  `outside_managed_roots`. Same-machine dual-path numbers therefore cannot be
  collected remotely today; the native-path samples above come from a second
  enrolled macOS device. A same-machine baseline needs either a managed
  project rooted outside the protected folders on device A (owner action:
  open a Herdr workspace on e.g. `~/herdr-bench`), or a local
  benchmark harness on that machine.

This baseline informs the design's conclusion rather than proving a win: the
broker-exec win is in *eliminating durable pane creation/occupancy* for
protected roots (each pane is a real terminal tab the user sees), not in
per-command latency, which is Edge-dominated anyway.

## 7. Proposed request/response contract (v1, tier 1)

**Implementation precondition.** This contract is a proposal, and the
`runtime_owner_token` field below **is not accepted by the current broker**:
`parse_request` in `crates/herdr-mcp/src/tcc_broker.rs` accepts exactly
`protocol`, `version`, `op`, `snapshot` and `args`, and rejects any other
top-level key. Before tier-1 exec lands, three things must be designed and
implemented together (tracked as the “`runtime_owner_token` support and
rotation” item in §9):

1. the token joins the request schema and the broker verifies it against the
   runtime owner record on every request;
2. a mid-connection rotation fails closed (no weaker fallback, no retry under
   the old token);
3. a broker restart loads only the current owner record and never authorizes a
   request with a token cached from before the restart.

Until all three exist, tier-1 exec mediation stays unimplemented. Note what the
current boundary really is (§3.1): a one-shot parent→child pipe, a
regular-file/signature check on the broker binary before spawn, and the request
schema allowlist. There is no socket, no peer-credential check and no caller
authentication today, so none of the layers above may be described as existing.

```jsonc
// request
{
  "runtime_owner_token": "...",
  "protocol": 6,                       // bumped from 5
  "op": "exec",                        // new allowlisted op
  "request_id": "req_...",
  "snapshot": {
    "program": "git",                  // allowlisted program name only
    "argv": ["status", "--porcelain=v1"],  // literal tokens, no shell
    "cwd": "~/Documents/herdr-mcp",
    "project_root": "~/Documents/herdr-mcp",
    "managed_roots": ["..."],          // runtime's current set
    "timeout_ms": 30000,
    "max_output_bytes": 4194304
  }
}
// response
{
  "ok": true,
  "exit_code": 0,
  "output": "...", "truncated": false,
  "elapsed_ms": 42,
  "session_id": "es_..."               // for read/kill continuity
}
```

Error codes mirror the runtime's: `outside_managed_roots`, `program_not_allowed`,
`flag_not_allowed`, `exec_timeout`, `exec_killed`, `broker_concurrency_limit`.

## 8. Rollout gates (not implemented by this issue)

1. Broker protocol revision bump + signing-script update (separate issue;
   signing changes are explicitly out of scope here).
2. Runtime integration: `utility_exec` gains `backend:"tcc_broker"` chosen
   when protected && broker compat_revision >= N, else `utility_pane` (fail
   closed, never fail to `native` on a protected root).
3. Rollback = disable the op server-side (allowlist entry removed) or fall
   back to `utility_pane`; no data migration exists.
4. Live verification: same-machine `backend:"tcc_broker"` latency samples vs
   the §6 baseline, on device A, after an unprotected-root benchmark workspace
   exists for the native column.

## 9. Open questions

- Should tier-2 signed descriptors reuse the #332 D-2 idempotency ledger for
  dedupe, or a broker-local seen-request table? (Leaning broker-local: ledger
  is a runtime concept.)
- Pane-occupancy vs broker-process cost under concurrent `herdr_exec` (10+
  parallel calls) — needs the real workload measurement once implemented.
- **`runtime_owner_token` support and rotation (§7 precondition).** The
  current broker request schema has no token field and performs no token check
  (`parse_request` accepts only `protocol`/`version`/`op`/`snapshot`/`args`), so
  this is a design-and-implementation gap, not a parameter to tune. Three
  things must be written down before tier-1 exec mediation lands:
  1. *Schema and per-request verification* — the token joins the request shape
     and the broker compares it with the runtime owner record on every request
     (§7).
  2. *Mid-connection rotation* — when the runtime writes a new owner token while
     a broker request is in flight, the request either fails closed as
     `owner_token_mismatch` (no weaker fallback, no retry on the old token) or
     the broker re-reads the owner record and accepts the new token. Leaning
     fail-closed: a request admitted under an older token must not silently
     continue under a newer authority.
  3. *Broker restart* — a restart must load the current owner record only; a
     stale token cached from before the restart may never authorize a request.
  The §7 precondition refers to this item; nothing here is implemented yet.
- Windows/Linux parity: broker is macOS-only; the `backend` contract must
  degrade identically on other OSes (they already take `native`).
