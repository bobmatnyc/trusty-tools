Documentation

- `version-control.md` no longer tells the agent to pass `--delete-branch` —
  the flag fails post-merge whenever a worktree holds the base branch (#7104)
  or the head branch (#8391), which every `isolation: "worktree"` delivery
  does. Guidance now points to `tm pr merge <n>` + `tm pr cleanup <n>`, with a
  fallback sequence and the correct merge-tree safety check for
  `git branch -D` (closes [#8391](https://github.com/bobmatnyc/trusty-tools/issues/8391))
