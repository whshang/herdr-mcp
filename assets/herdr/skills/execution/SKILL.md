---
name: execution
description: Choose bounded short execution or durable long sessions with herdr_exec and herdr_exec_start/read/kill using start-once and delta-read semantics.
---

# Execution

Own: `herdr_exec`, `herdr_exec_start`, `herdr_exec_read`, `herdr_exec_kill`.

Use `herdr_exec` for bounded deterministic commands expected to finish in one call. Use `herdr_exec_start` for non-trivial tests, builds, servers, benchmarks, network-bound file transfers, or work that may outlive a Web turn. Treat GitHub Actions artifact downloads (`gh api` / `gh run download`), `curl`/`wget`, `scp`/`rsync`, and comparable uploads/downloads as durable-session work by default when remote latency or artifact size can push them beyond the synchronous budget.

Within one WebChat conversation/task, keep bounded shell work for the same workspace/project on the canonical `herdr-mcp:utility` pane. Repeated `herdr_exec` calls already reuse that pane; do not open or split another pane merely to issue the next shell command. When several independent short commands for the same boundary are already known, combine them into one readable shell wave with grouped output. A different workspace may own its own canonical utility pane; do not force unrelated projects through one pane.

`herdr_exec_start` is a durable background-process path, not a request for another visible terminal. Never use it to obtain a fresh pane or to avoid a bounded `herdr_exec`. For bounded SSH maintenance or recovery, stay on the controlling workspace's canonical utility pane; when the remote work is genuinely long, start exactly one durable session and keep polling that same `session_id`.

For long work: start once, retain `session_id`, read again from `next_offset`, and resume the same session across turns. Do not restart at offset 0 unless earlier output is required. Use `phase`, `progress`, `running`, exit/signal, and bounded output together; kill only the known session that should be cancelled.

If a short command may already have been delivered and its outcome is uncertain, observe pane/process/file/Git effects before any retry. If a synchronous transfer times out, inspect the original process/session and destination-file growth before any replay. For long work, reconnect to the existing session before considering a replacement process.

Process completion is only process-level evidence. Verify the task's relevant files/diff, test assertion, artifact, runtime state, or other affected boundary before declaring completion.
