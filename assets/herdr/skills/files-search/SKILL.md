---
name: files-search
description: Perform bounded project reads, directory listing, native text search, and image inspection with herdr_fs_read/list/grep/image.
---

# Files Search

Own: `herdr_fs_read`, `herdr_fs_list`, `herdr_fs_grep`, `herdr_fs_image`.

Use the smallest scope that answers the question. Prefer targeted grep/list/read over whole-tree or whole-file ingestion; group independent reads into one wave and reuse known roots/paths.

Use bounded line/byte windows and narrow subsequent reads from compact results. Treat search truncation as a signal to reduce scope, not to repeat the same broad query. Use literal search unless regex is required; add file globs/match limits when they materially narrow work.

`herdr_fs_grep` may use an `rg` fast path or safe fallback; depend on the behavior contract, not a fixed backend. Use `herdr_fs_image` only for targeted visual inspection of images already inside a managed Git root.

Managed-root, secret-path, symlink, and byte-limit boundaries remain runtime-enforced. Read-only investigation needs specific evidence, not a new worktree or coding-agent merely to perform file IO.
