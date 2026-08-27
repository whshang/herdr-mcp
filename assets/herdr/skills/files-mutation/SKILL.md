---
name: files-mutation
description: Select and safely apply repository file edits, writes, and patches with herdr_fs_edit, herdr_fs_write, and herdr_fs_patch while preserving managed-root, dirty, busy, and mutation-evidence boundaries.
---

# Files Mutation

Own these public tools:

```text
herdr_fs_edit
herdr_fs_write
herdr_fs_patch
```

## Tool selection

- Use `herdr_fs_edit` for one exact, unique replacement in an existing file.
- Use `herdr_fs_write` for a new file or an intentional full rewrite.
- Prefer `herdr_fs_patch` for coherent multi-hunk or multi-file changes and when preflight across all targets matters.
- Read the exact target context before a mutation when current content is not already known.

## Mutation discipline

Keep dependent mutations ordered. Parallel mutation is allowed only when ownership and isolation are explicit and the target files do not overlap.

Treat `confirm_dirty` and `confirm_busy` as acknowledgement of an already understood condition. They do not authorize overwriting unrelated work or taking ownership from another lane.

For patches, preflight all targets before applying. Preserve the runtime's atomic/transactional expectations; a partial-looking client response is not permission to reconstruct or replay the mutation manually.

## Uncertain outcomes

If a mutation times out or delivery becomes uncertain:

1. inspect the target file/Git state;
2. determine whether the intended mutation already applied;
3. retry only when evidence proves it did not apply.

Never blind-retry a write, edit, or patch.

## Authorization boundary

Loading this Skill provides policy only. Managed-root/path confinement, secret paths, dirty/busy gates, symlink checks, and mutation fencing remain runtime-enforced.

## Completion evidence

Verify the resulting file content or Git diff, then run the smallest relevant validation. For multi-file changes, confirm all intended targets changed and unrelated files did not.
