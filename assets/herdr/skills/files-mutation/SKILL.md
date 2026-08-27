---
name: files-mutation
description: Select and safely apply repository edits, writes, and transactional patches with herdr_fs_edit, herdr_fs_write, and herdr_fs_patch.
---

# Files Mutation

Own: `herdr_fs_edit`, `herdr_fs_write`, `herdr_fs_patch`.

## Tool selection

- `herdr_fs_edit`: one exact unique replacement in an existing file.
- `herdr_fs_write`: new file or intentional full rewrite.
- `herdr_fs_patch`: coherent multi-hunk/multi-file changes and transaction-style preflight.

Read exact target context when current content is not already known. For patches, preflight every target before applying.

`confirm_dirty` and `confirm_busy` acknowledge a verified condition; they do not grant ownership over unrelated work. Parallel file mutation requires explicit non-overlapping ownership/isolation.

If delivery/outcome is uncertain, inspect file/Git state and retry only when evidence proves the mutation did not apply. Do not reconstruct a partial-looking patch manually.

Managed-root/path confinement, secret-path checks, dirty/busy gates, symlink checks, and mutation fencing remain runtime-enforced; loading this Skill grants no authorization.

Verify resulting content/diff and run the smallest relevant validation. Multi-file operations must confirm every intended target and no unrelated change.
