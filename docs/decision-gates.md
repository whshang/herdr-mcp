# Task decision gates

A decision gate is a durable, blocking choice on one non-terminal task. It is
independent of worker liveness and existing approval/grant semantics. It does
not authorize merges, releases or deployments. Bot/agent approval never counts
as the owner's approval.

Use the existing `herdr_call` local method surface:

- `herdr_mcp.agent.task.gate.create`: `task_id`, `question`, `options`.
- `herdr_mcp.agent.task.gate.status`: `gate_id`.
- `herdr_mcp.agent.task.gate.cancel`: `gate_id`, from the creating parent scope.
- `herdr_mcp.agent.task.gate.resolve`: always refuses with
  `gate_resolution_requires_human`, including trusted local IPC, MCP, WebChat,
  browser actuation and agent callers. No request field can assert human identity.

Creation requires a visible non-terminal task and permits one open gate per task.
A question is 1–1024 UTF-8 bytes; options are 2–8 distinct non-empty strings of
at most 128 UTF-8 bytes each. Blank strings and control characters are rejected.
WebChat scope comes from the existing proven caller-session binding; missing
binding fails closed. A supplied parent ref cannot establish gate ownership.
Trusted local callers use the local parent scope; a gate created in a WebChat
session can only be cancelled from that same session.

While open, task status and both full/compact inbox rows include the gate and
`blocked_on_gate:true`. The task remains non-terminal and cannot be acked.
`worker_done` is still recorded exactly once, but completion/parent notification
waits until release. Lifecycle settle events remain pending while the gate is
open. Resolution or cancellation releases the hold on the next task reconciliation;
it never dispatches new work. Existing boot/cursor fences still apply to lifecycle
inference. Explicit worker reports remain durable across runtime restart.

The local CLI also supports creation, inspection and cancellation:

```sh
herdr-mcp agent gate create --task-id TASK --question 'Which path?' --option continue --option stop
herdr-mcp agent gate status --gate-id GATE
herdr-mcp agent gate cancel --gate-id GATE
```

Only the owner in an independent local interactive terminal may resolve a gate:

```sh
herdr-mcp agent gate resolve --gate-id GATE --resolution continue
```

The CLI opens the controlling `/dev/tty`, requires the foreground process group,
and asks the owner to retype the chosen option exactly. Stdin/pipes cannot provide
confirmation. A managed Herdr pane/agent environment or `HERDR_MCP_EXEC_ID`
refuses before any store access. The gate records the chosen option, resolution
time and `resolved_by:{"class":"local_cli_interactive"}`. Invalid options,
mismatched confirmation and repeated resolution change no gate state.

This is **best-effort local attestation, not cryptographic proof**. A program
controlling the OS account could remove environment markers or emulate a TTY.
Agents must never run the resolver on behalf of a human. Platforms without a
supported controlling TTY fail closed. No extension or resolution UI is added.

The resolver updates SQLite directly; no runtime RPC accepts an attestation.
It uses `HERDR_MCP_STATE_DIR/state.db` when set, otherwise the selected instance's
config directory (`RuntimePaths`). For a development candidate with a separate
state directory, set `HERDR_MCP_STATE_DIR` to that exact candidate directory in
the owner's independent terminal. It refuses a missing database.

Schema migration 16 adds `task_decision_gates` and a unique open-gate index.
Open gates retain their task through ordinary ledger expiry; closed gates expire
with the associated task. Gate-free legacy task JSON stays unchanged. Schema-15
binaries refuse the migrated database: ignoring a gate on rollback would bypass
a blocking decision. Rollback requires a pre-migration backup; no service,
installation or rollback is performed by this feature.
