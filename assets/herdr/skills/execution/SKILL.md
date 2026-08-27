---
name: execution
description: Run bounded short commands and durable long execution sessions with herdr_exec, herdr_exec_start, herdr_exec_read, and herdr_exec_kill while preserving start-once and delta-read semantics.
---

# Execution

Own these public tools:

```text
herdr_exec
herdr_exec_start
herdr_exec_read
herdr_exec_kill
```

## Short versus long execution

Use `herdr_exec` for bounded deterministic commands that should finish inside one call and benefit from the visible utility pane.

Use `herdr_exec_start` for non-trivial tests, builds, servers, benchmarks, or other work that may outlive one Web turn. Start once, retain the returned session identity, and continue with `herdr_exec_read`.

## Session continuation

- Read from `next_offset`/the last returned offset. Do not restart from zero unless earlier output is required again.
- Use `phase`, `progress`, `running`, exit code, and bounded output together. A process exit may still require Git/file/test/artifact evidence for the higher-level task.
- Resume the same session across user turns; a new user message does not justify starting the command again.
- Kill only the known session that should be cancelled. Verify its resulting state when cancellation matters.

## Uncertain execution

If a short execution may already have been delivered and the control response is uncertain, do not rerun the command blindly. Observe its effects or pane/process state first.

Long-session identity is the retry boundary: reconnect and read the existing session before considering a replacement process.

## Output discipline

Keep commands and output bounded. Prefer targeted test/build commands during iteration, then broaden validation when the change is stable. Use compact summaries and focused follow-up reads rather than repeatedly requesting full retained output.

## Completion evidence

Execution success proves the process-level result only. Combine it with the relevant task evidence: expected files/diff, test assertions, generated artifact, service state, or other explicit boundary verification.
