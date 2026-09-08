---
name: execution
description: Choose bounded short execution or durable long sessions with herdr_exec and herdr_exec_start/read/kill using start-once and delta-read semantics.
---

# Execution

Own: `herdr_exec`, `herdr_exec_start`, `herdr_exec_read`, `herdr_exec_kill`.

Use `herdr_exec` for bounded deterministic commands expected to finish in one call. Use `herdr_exec_start` for non-trivial tests, builds, servers, benchmarks, network-bound file transfers, or work that may outlive a Web turn. Treat GitHub Actions artifact downloads (`gh api` / `gh run download`), `curl`/`wget`, `scp`/`rsync`, and comparable uploads/downloads as durable-session work by default when remote latency or artifact size can push them beyond the synchronous budget.

For long work: start once, retain `session_id`, read again from `next_offset`, and resume the same session across turns. Do not restart at offset 0 unless earlier output is required. Use `phase`, `progress`, `running`, exit/signal, and bounded output together; kill only the known session that should be cancelled.

If a short command may already have been delivered and its outcome is uncertain, observe pane/process/file/Git effects before any retry. If a synchronous transfer times out, inspect the original process/session and destination-file growth before any replay. For long work, reconnect to the existing session before considering a replacement process.

Process completion is only process-level evidence. Verify the task's relevant files/diff, test assertion, artifact, runtime state, or other affected boundary before declaring completion.
