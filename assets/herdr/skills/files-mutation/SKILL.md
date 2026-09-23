---
name: files-mutation
description: Select and safely apply repository edits, writes, and transactional patches with herdr_fs_edit, herdr_fs_write, and herdr_fs_patch.
---

# Files Mutation

Own: `herdr_fs_edit`, `herdr_fs_write`, `herdr_fs_patch`.

## Tool selection

- `herdr_fs_edit`: EDIT EXISTING FILE via one exact unique replacement.
- `herdr_fs_write`: CREATE NEW FILE / FULL REWRITE only; it is not a read or generic file-operation tool.
- `herdr_fs_patch`: PATCH EXISTING FILES for coherent multi-hunk/multi-file changes and transaction-style preflight.

Read exact target context when current content is not already known. For patches, preflight every target before applying.

Read the target/diff before setting `confirm_dirty` or `confirm_busy`. Omit/leave them false until the corresponding condition has been observed and current-task ownership is safe. They acknowledge a verified condition; they do not grant ownership over unrelated work. Parallel file mutation requires explicit non-overlapping ownership/isolation.

Never use `herdr_fs_write` as a shortcut around exact-match or patch preflight. Full rewrite is appropriate only when replacing the whole file is intentional and the current content is already accounted for.

If delivery/outcome is uncertain, inspect file/Git state and retry only when evidence proves the mutation did not apply. Do not reconstruct a partial-looking patch manually.

Managed-root/path confinement, secret-path checks, dirty/busy gates, symlink checks, and mutation fencing remain runtime-enforced; loading this Skill grants no authorization.

Verify resulting content/diff and run the smallest relevant validation. Multi-file operations must confirm every intended target and no unrelated change.
