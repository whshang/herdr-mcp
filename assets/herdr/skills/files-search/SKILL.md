---
name: files-search
description: Perform bounded project file reads, directory listing, native text search, and repository image reads with herdr_fs_read, herdr_fs_list, herdr_fs_grep, and herdr_fs_image.
---

# Files Search

Own these public tools:

```text
herdr_fs_read
herdr_fs_list
herdr_fs_grep
herdr_fs_image
```

## Read strategy

- Use the smallest scope that can answer the question. Prefer targeted grep/list/read over whole-tree or whole-file ingestion.
- Form a small independent read wave when several facts can be gathered without depending on each other.
- Reuse known project roots and paths. Do not repeat project discovery for each read.
- Use bounded line/byte windows, then continue from the next relevant range only when needed.
- When a compact search result identifies the right files, narrow subsequent reads to those files instead of repeating a broad search.

## Search

`herdr_fs_grep` owns native project search. The runtime may use an `rg` fast path or a safe fallback; the Skill depends on the behavior contract rather than a fixed backend.

Choose a literal pattern unless regex semantics are required. Use a filename glob and bounded match count when they materially narrow work. Treat truncation as evidence to narrow scope, not as a reason to rerun the same broad search unchanged.

## Images

Use `herdr_fs_image` for images already inside a managed Git root when visual inspection is required. Keep image reads bounded and targeted.

## Safety and evidence

Managed-root, secret-path, symlink, and byte-limit boundaries are runtime-enforced. Reading a Skill does not weaken them.

A read-only investigation is complete when the required facts are supported by specific file/search evidence. It does not require a new worktree or coding-agent delegation solely to perform reads.
